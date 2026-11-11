#![allow(dead_code)]

use arceos_posix_api as api;
use axerrno::LinuxError;
use axhal::arch::TrapFrame;
use axhal::paging::MappingFlags;
use axhal::trap::{register_trap_handler, SYSCALL};
use axtask::current;
use axtask::TaskExtRef;
use core::ffi::{c_char, c_int, c_void};

const SYS_IOCTL: usize = 29;
const SYS_OPENAT: usize = 56;
const SYS_CLOSE: usize = 57;
const SYS_READ: usize = 63;
const SYS_WRITE: usize = 64;
const SYS_WRITEV: usize = 66;
const SYS_EXIT: usize = 93;
const SYS_EXIT_GROUP: usize = 94;
const SYS_SET_TID_ADDRESS: usize = 96;
const SYS_MMAP: usize = 222;

const AT_FDCWD: i32 = -100;

/// Macro to generate syscall body
///
/// It will receive a function which return Result<_, LinuxError> and convert it to
/// the type which is specified by the caller.
#[macro_export]
macro_rules! syscall_body {
    ($fn: ident, $($stmt: tt)*) => {{
        #[allow(clippy::redundant_closure_call)]
        let res = (|| -> axerrno::LinuxResult<_> { $($stmt)* })();
        match res {
            Ok(_) | Err(axerrno::LinuxError::EAGAIN) => debug!(concat!(stringify!($fn), " => {:?}"),  res),
            Err(_) => info!(concat!(stringify!($fn), " => {:?}"), res),
        }
        match res {
            Ok(v) => v as _,
            Err(e) => {
                -e.code() as _
            }
        }
    }};
}

bitflags::bitflags! {
    #[derive(Debug)]
    /// permissions for sys_mmap
    ///
    /// See <https://github.com/bminor/glibc/blob/master/bits/mman.h>
    struct MmapProt: i32 {
        /// Page can be read.
        const PROT_READ = 1 << 0;
        /// Page can be written.
        const PROT_WRITE = 1 << 1;
        /// Page can be executed.
        const PROT_EXEC = 1 << 2;
    }
}

impl From<MmapProt> for MappingFlags {
    fn from(value: MmapProt) -> Self {
        let mut flags = MappingFlags::USER;
        if value.contains(MmapProt::PROT_READ) {
            flags |= MappingFlags::READ;
        }
        if value.contains(MmapProt::PROT_WRITE) {
            flags |= MappingFlags::WRITE;
        }
        if value.contains(MmapProt::PROT_EXEC) {
            flags |= MappingFlags::EXECUTE;
        }
        flags
    }
}

bitflags::bitflags! {
    #[derive(Debug)]
    /// flags for sys_mmap
    ///
    /// See <https://github.com/bminor/glibc/blob/master/bits/mman.h>
    struct MmapFlags: i32 {
        /// Share changes
        const MAP_SHARED = 1 << 0;
        /// Changes private; copy pages on write.
        const MAP_PRIVATE = 1 << 1;
        /// Map address must be exactly as requested, no matter whether it is available.
        const MAP_FIXED = 1 << 4;
        /// Don't use a file.
        const MAP_ANONYMOUS = 1 << 5;
        /// Don't check for reservations.
        const MAP_NORESERVE = 1 << 14;
        /// Allocation is for a stack.
        const MAP_STACK = 0x20000;
    }
}

#[register_trap_handler(SYSCALL)]
fn handle_syscall(tf: &TrapFrame, syscall_num: usize) -> isize {
    ax_println!("handle_syscall [{}] ...", syscall_num);
    let ret = match syscall_num {
        SYS_IOCTL => sys_ioctl(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _) as _,
        SYS_SET_TID_ADDRESS => sys_set_tid_address(tf.arg0() as _),
        SYS_OPENAT => sys_openat(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
        ),
        SYS_CLOSE => sys_close(tf.arg0() as _),
        SYS_READ => sys_read(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        SYS_WRITE => sys_write(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        SYS_WRITEV => sys_writev(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        SYS_EXIT_GROUP => {
            ax_println!("[SYS_EXIT_GROUP]: system is exiting ..");
            axtask::exit(tf.arg0() as _)
        }
        SYS_EXIT => {
            ax_println!("[SYS_EXIT]: system is exiting ..");
            axtask::exit(tf.arg0() as _)
        }
        SYS_MMAP => sys_mmap(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
            tf.arg5() as _,
        ),
        _ => {
            ax_println!("Unimplemented syscall: {}", syscall_num);
            -LinuxError::ENOSYS.code() as _
        }
    };
    ret
}

fn sys_mmap(
    addr: *mut usize,
    length: usize,
    prot: i32,
    flags: i32,
    fd: i32,
    offset: isize,
) -> isize {
    use memory_addr::{VirtAddr, VirtAddrRange, PAGE_SIZE_4K};

    syscall_body!(sys_mmap, {
        let prot = MmapProt::from_bits(prot).ok_or(LinuxError::EINVAL)?;
        let flags = MmapFlags::from_bits(flags).ok_or(LinuxError::EINVAL)?;
        if length == 0 || offset < 0 || offset as usize % PAGE_SIZE_4K != 0 {
            return Err(LinuxError::EINVAL);
        }
        // Shared writeback and replacing existing mappings are not implemented.
        if !flags.contains(MmapFlags::MAP_PRIVATE)
            || flags.intersects(MmapFlags::MAP_SHARED | MmapFlags::MAP_FIXED)
        {
            return Err(LinuxError::EOPNOTSUPP);
        }
        let size = length
            .checked_add(PAGE_SIZE_4K - 1)
            .ok_or(LinuxError::ENOMEM)?
            & !(PAGE_SIZE_4K - 1);
        let file = if flags.contains(MmapFlags::MAP_ANONYMOUS) {
            None
        } else {
            Some(api::get_file_like(fd)?)
        };
        let curr = current();
        let mut aspace = curr.task_ext().aspace.lock();
        if size > aspace.size() {
            return Err(LinuxError::ENOMEM);
        }
        let limit = VirtAddrRange::from_start_size(aspace.base(), aspace.size());
        // Avoid NULL and the application image when no address hint is supplied.
        let hint = (addr as usize)
            .max(0x1000_0000)
            .checked_add(PAGE_SIZE_4K - 1)
            .ok_or(LinuxError::ENOMEM)?
            & !(PAGE_SIZE_4K - 1);
        let hint = VirtAddr::from(hint);
        let start = aspace
            .find_free_area(hint, size, limit)
            .ok_or(LinuxError::ENOMEM)?;
        aspace.map_alloc(start, size, prot.into(), true)?;
        if let Some(file) = file {
            let saved = api::sys_lseek(fd, 0, 1);
            if saved < 0 {
                let _ = aspace.unmap(start, size);
                return Err(LinuxError::ESPIPE);
            }
            let result = (|| -> axerrno::LinuxResult<()> {
                let seek = api::sys_lseek(fd, offset as _, 0);
                if seek < 0 {
                    return Err(LinuxError::EINVAL);
                }
                let mut buffer = [0u8; PAGE_SIZE_4K];
                let mut copied = 0;
                while copied < length {
                    let count = buffer.len().min(length - copied);
                    let n = file.read(&mut buffer[..count])?;
                    if n == 0 {
                        break;
                    }
                    aspace.write(start + copied, &buffer[..n])?;
                    copied += n;
                }
                Ok(())
            })();
            api::sys_lseek(fd, saved, 0);
            if let Err(error) = result {
                let _ = aspace.unmap(start, size);
                return Err(error);
            }
        }
        Ok(start.as_usize())
    })
}

fn sys_openat(dfd: c_int, fname: *const c_char, flags: c_int, mode: api::ctypes::mode_t) -> isize {
    assert_eq!(dfd, AT_FDCWD);
    api::sys_open(fname, flags, mode) as isize
}

fn sys_close(fd: i32) -> isize {
    api::sys_close(fd) as isize
}

fn sys_read(fd: i32, buf: *mut c_void, count: usize) -> isize {
    api::sys_read(fd, buf, count)
}

fn sys_write(fd: i32, buf: *const c_void, count: usize) -> isize {
    api::sys_write(fd, buf, count)
}

fn sys_writev(fd: i32, iov: *const api::ctypes::iovec, iocnt: i32) -> isize {
    unsafe { api::sys_writev(fd, iov, iocnt) }
}

fn sys_set_tid_address(tid_ptd: *const i32) -> isize {
    let curr = current();
    curr.task_ext().set_clear_child_tid(tid_ptd as _);
    curr.id().as_u64() as isize
}

fn sys_ioctl(_fd: i32, _op: usize, _argp: *mut c_void) -> i32 {
    ax_println!("Ignore SYS_IOCTL");
    0
}
