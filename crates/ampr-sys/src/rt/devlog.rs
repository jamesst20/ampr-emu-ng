//! `/devlog` for fake-mount titles.
//!
//! The system mounts `/devlog` into an installed app's sandbox; ShadowMount+
//! mounts the game folder, `/data` and the USB roots but not `/devlog`, so a
//! game that writes its log or its shader cache there gets ENOENT for every
//! path. Kingdom Come Deliverance 2 (PPSA21704) is one: without `/devlog` it
//! stops at the end of the intro cinematic, with it the game reaches the
//! level load.
//!
//! The runtime cannot create `/devlog` itself: on 13.00 the mkdir of a
//! directory at the sandbox root fails EACCES even though the root lists as
//! root:root 0775 and the game's files are group 0 - the console's sandbox
//! policy, not the mode bits (`B D 0xd` in the black box). `/data` is a
//! writable mount, so the runtime keeps the tree at `/data/ampr_devlog`
//! instead and `map_path` rewrites the game's `/devlog...` paths to it in
//! every path hook, `mkdir` included (rt/index.rs `posix_mkdir_emul`), so the
//! game creates its own log and shader-cache directories there.
//!
//! Nothing is created when `/devlog` already exists (a native install), and
//! only the root of the fallback tree is made here: the game's own mkdir
//! calls build the rest through the redirect.

use super::sys::{self, LazySym};
use core::ffi::{c_char, c_int, CStr};
use core::sync::atomic::{AtomicBool, Ordering};

/// The writable tree the game's `/devlog` paths are served from.
const FALLBACK: &[u8] = b"/data/ampr_devlog";
const FALLBACK_C: &CStr = c"/data/ampr_devlog";

// libkernel exports the FreeBSD `mkdir` on the systems the other paths use
// (the runtime resolves `open`, `read`, `stat` the same way); `sceKernelMkdir`
// is the other name in its stub library.
static MKDIR: LazySym = LazySym::new(c"mkdir");
static SCE_MKDIR: LazySym = LazySym::new(c"sceKernelMkdir");

/// Set when the game's `/devlog` paths must go to `FALLBACK`.
static REDIRECT: AtomicBool = AtomicBool::new(false);

/// `EEXIST`: the directory is already there.
const EEXIST: c_int = 17;
/// No mkdir entry point resolved at all.
#[cfg_attr(not(feature = "mem_report"), allow(dead_code))]
const NO_SYMBOL: c_int = -1;

type MkdirFn = unsafe extern "C" fn(*const c_char, c_int) -> c_int;

/// `mkdir(path, 0777)`; the errno, 0 when it exists or was created, -1 when
/// libkernel exported neither name.
unsafe fn mkdir_one(mkdir: MkdirFn, path: &CStr) -> c_int {
    if unsafe { mkdir(path.as_ptr(), 0o777) } == 0 {
        return 0;
    }
    let e = sys::errno();
    if e == EEXIST {
        0
    } else if e == 0 {
        0x7fff
    } else {
        e
    }
}

/// Prepares the tree; safe to call in any title and on any firmware.
pub(crate) unsafe fn ensure() {
    let mkdir = unsafe { MKDIR.get::<MkdirFn>() }
        .or_else(|| unsafe { SCE_MKDIR.get::<MkdirFn>() });
    let Some(mkdir) = mkdir else {
        #[cfg(feature = "mem_report")]
        unsafe {
            super::blackbox::note(b'B', b'D', NO_SYMBOL as i64, c"/devlog".as_ptr());
            super::exports::notify(b"AMPR devlog: no mkdir in libkernel");
        }
        return;
    };
    // A real /devlog (native install, or a mounter that provides it) is used
    // as is.
    let root = unsafe { mkdir_one(mkdir, c"/devlog") };
    let mut fallback = 0;
    if root != 0 {
        fallback = unsafe { mkdir_one(mkdir, FALLBACK_C) };
        if fallback == 0 {
            REDIRECT.store(true, Ordering::Release);
        }
    }
    #[cfg(feature = "mem_report")]
    unsafe {
        // "B D <rc> /devlog" and "B F <rc> /data/ampr_devlog": 0 present or
        // created, an errno otherwise, -1 when libkernel exported neither
        // name.
        super::blackbox::note(b'B', b'D', root as i64, c"/devlog".as_ptr());
        super::blackbox::note(b'B', b'F', fallback as i64, FALLBACK_C.as_ptr());
        let mut msg = [0u8; 64];
        let head = b"AMPR devlog: root=0x";
        msg[..head.len()].copy_from_slice(head);
        let mut n = head.len();
        for shift in (0..8).rev() {
            msg[n] = b"0123456789abcdef"[((root as u32 >> (shift * 4)) & 0xf) as usize];
            n += 1;
        }
        let mid = b" data=0x";
        msg[n..n + mid.len()].copy_from_slice(mid);
        n += mid.len();
        for shift in (0..8).rev() {
            msg[n] = b"0123456789abcdef"[((fallback as u32 >> (shift * 4)) & 0xf) as usize];
            n += 1;
        }
        super::exports::notify(&msg[..n]);
    }
    #[cfg(not(feature = "mem_report"))]
    let _ = (root, fallback);
}

/// Rewrites a `/devlog...` path to the `/data/ampr_devlog...` tree when the
/// sandbox refused the real one; `path` itself otherwise. `buf` must outlive
/// the call (256 bytes covers the game's longest shader cache path).
pub(crate) unsafe fn map_path<'a>(path: *const c_char, buf: &'a mut [u8; 256]) -> *const c_char {
    if path.is_null() || !REDIRECT.load(Ordering::Relaxed) {
        return path;
    }
    const PREFIX: &[u8] = b"/devlog";
    for (i, &b) in PREFIX.iter().enumerate() {
        if unsafe { *path.add(i) } as u8 != b {
            return path;
        }
    }
    let next = unsafe { *path.add(PREFIX.len()) } as u8;
    if next != 0 && next != b'/' {
        return path;
    }
    let mut n = 0;
    for &b in FALLBACK {
        buf[n] = b;
        n += 1;
    }
    let mut j = PREFIX.len();
    loop {
        if n + 1 >= buf.len() {
            break;
        }
        let b = unsafe { *path.add(j) } as u8;
        buf[n] = b;
        n += 1;
        if b == 0 {
            break;
        }
        j += 1;
    }
    buf.as_ptr().cast()
}
