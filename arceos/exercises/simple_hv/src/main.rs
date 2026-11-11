#![cfg_attr(feature = "axstd", no_std)]
#![cfg_attr(feature = "axstd", no_main)]
#![feature(asm_const)]
#![feature(riscv_ext_intrinsics)]

extern crate alloc;
#[cfg(feature = "axstd")]
extern crate axstd as std;
#[macro_use]
extern crate axlog;

mod csrs;
mod loader;
mod regs;
mod sbi;
mod task;
mod vcpu;

use crate::regs::GprIndex::{A0, A1};
use alloc::boxed::Box;
use axhal::mem::PhysAddr;
use csrs::defs::hstatus;
use csrs::{RiscvCsrTrait, CSR};
use loader::load_vm_image;
use riscv::register::{scause, sstatus, stval};
use sbi::SbiMessage;
use tock_registers::LocalRegisterCopy;
use vcpu::VmCpuRegisters;
use vcpu::_run_guest;

const VM_ENTRY: usize = 0x8020_0000;

// Sv39x4 uses a 16 KiB root table, unlike the host's 4 KiB Sv39 root.
#[repr(C, align(16384))]
struct GuestPageTable([u64; 2048]);

#[cfg_attr(feature = "axstd", no_mangle)]
fn main() {
    ax_println!("Hypervisor ...");

    // A new address space for vm.
    let mut uspace = axmm::new_user_aspace().unwrap();

    // Load vm binary file into address space.
    if let Err(e) = load_vm_image("/sbin/skernel2", &mut uspace) {
        panic!("Cannot load app! {:?}", e);
    }

    // Setup context to prepare to enter guest mode.
    let mut ctx = VmCpuRegisters::default();
    prepare_guest_context(&mut ctx);

    // Setup pagetable for 2nd address mapping.
    let ept_root = uspace.page_table_root();
    let _guest_root = prepare_vm_pgtable(ept_root);

    // Kick off vm and wait for it to exit.
    while !run_guest(&mut ctx) {}

    unsafe {
        core::arch::asm!("csrw hgatp, zero");
        core::arch::riscv64::hfence_gvma_all();
    }

    ax_println!("Hypervisor ok!");
}

fn prepare_vm_pgtable(ept_root: PhysAddr) -> Box<GuestPageTable> {
    let mut root = Box::new(GuestPageTable([0; 2048]));
    unsafe {
        core::ptr::copy_nonoverlapping(
            axhal::mem::phys_to_virt(ept_root).as_ptr().cast::<u64>(),
            root.0.as_mut_ptr(),
            512,
        );
    }
    let root_paddr = axhal::mem::virt_to_phys((root.0.as_ptr() as usize).into());
    let hgatp = 8usize << 60 | usize::from(root_paddr) >> 12;
    unsafe {
        core::arch::asm!(
            "csrw hgatp, {hgatp}",
            hgatp = in(reg) hgatp,
        );
        core::arch::riscv64::hfence_gvma_all();
    }
    root
}

fn run_guest(ctx: &mut VmCpuRegisters) -> bool {
    // A timer interrupt during the assembly transition would enter the guest
    // exit vector with partially restored host registers. Mask HS interrupts
    // until host state and the exit cause have both been recovered.
    let irqs_enabled = axhal::arch::irqs_enabled();
    axhal::arch::disable_irqs();
    let saved_sie: usize;
    unsafe {
        core::arch::asm!("csrrw {}, sie, zero", out(reg) saved_sie);
        _run_guest(ctx);
    }
    let stopped = vmexit_handler(ctx);
    unsafe {
        core::arch::asm!("csrw sie, {}", in(reg) saved_sie);
    }
    if irqs_enabled {
        axhal::arch::enable_irqs();
    }
    stopped
}

#[allow(unreachable_code)]
fn vmexit_handler(ctx: &mut VmCpuRegisters) -> bool {
    use scause::{Exception, Trap};

    let scause = scause::read();
    match scause.cause() {
        Trap::Exception(Exception::VirtualSupervisorEnvCall) => {
            let sbi_msg = SbiMessage::from_regs(ctx.guest_regs.gprs.a_regs()).ok();
            ax_println!("VmExit Reason: VSuperEcall: {:?}", sbi_msg);
            if let Some(msg) = sbi_msg {
                match msg {
                    SbiMessage::Reset(_) => {
                        let a0 = ctx.guest_regs.gprs.reg(A0);
                        let a1 = ctx.guest_regs.gprs.reg(A1);
                        ax_println!("a0 = {:#x}, a1 = {:#x}", a0, a1);
                        assert_eq!(a0, 0x6688);
                        assert_eq!(a1, 0x1234);
                        ax_println!("Shutdown vm normally!");
                        return true;
                    }
                    _ => todo!(),
                }
            } else {
                panic!("bad sbi message! ");
            }
        }
        Trap::Exception(Exception::IllegalInstruction) => {
            // mhartid is machine-only. Supply the virtual hart identity when
            // the guest reads it with CSRRS rd, mhartid, zero.
            let instruction = stval::read();
            assert_eq!(
                instruction & 0xffff_f07f,
                0xf140_2073,
                "unsupported guest instruction"
            );
            let rd = regs::GprIndex::from_raw(((instruction >> 7) & 31) as u32).unwrap();
            ctx.guest_regs.gprs.set_reg(rd, 0x1234);
            ctx.guest_regs.sepc += 4;
        }
        Trap::Exception(Exception::LoadGuestPageFault) => {
            // The guest's ld a0, 64(zero) reads a virtual device register.
            // Emulate this MMIO access without mapping the zero page as RAM.
            assert_eq!(stval::read(), 0x40, "unsupported guest MMIO address");
            ctx.guest_regs.gprs.set_reg(A0, 0x6688);
            ctx.guest_regs.sepc += 4;
        }
        _ => {
            panic!(
                "Unhandled trap: {:?}, sepc: {:#x}, stval: {:#x}",
                scause.cause(),
                ctx.guest_regs.sepc,
                stval::read()
            );
        }
    }
    false
}

fn prepare_guest_context(ctx: &mut VmCpuRegisters) {
    // Set hstatus
    let mut hstatus =
        LocalRegisterCopy::<usize, hstatus::Register>::new(riscv::register::hstatus::read().bits());
    // Set Guest bit in order to return to guest mode.
    hstatus.modify(hstatus::spv::Guest);
    // Set SPVP bit in order to accessing VS-mode memory from HS-mode.
    hstatus.modify(hstatus::spvp::Supervisor);
    CSR.hstatus.write_value(hstatus.get());
    ctx.guest_regs.hstatus = hstatus.get();

    // Set sstatus in guest mode.
    let mut sstatus = sstatus::read();
    sstatus.set_spp(sstatus::SPP::Supervisor);
    ctx.guest_regs.sstatus = sstatus.bits() & !(1 << 1);
    // Return to entry to start vm.
    ctx.guest_regs.sepc = VM_ENTRY;
}
