// Picks the GTK renderer. On software OpenGL (llvmpipe, e.g. a VM without
// 3D acceleration) GTK's default GL renderer emulates the GPU on the CPU and
// runs several times slower than the cairo renderer, which rasterizes on
// the CPU directly. Measured on a VMware VM with llvmpipe while the terminal
// scrolls: ngl 13 fps, gl 44 fps, cairo 52 fps.

use std::ffi::CStr;
use std::os::raw::c_char;

use gtk::{gdk, prelude::*};

const GL_RENDERER: u32 = 0x1F01;

#[link(name = "epoxy")]
extern "C" {
    /// libepoxy's dispatch pointer for glGetString (GTK links libepoxy).
    static epoxy_glGetString: Option<unsafe extern "C" fn(u32) -> *const c_char>;
}

/// OpenGL renderer name of the display, e.g. "llvmpipe (LLVM 20.1.2)".
fn gl_renderer(display: &gdk::Display) -> Option<String> {
    let ctx = display.create_gl_context().ok()?;
    ctx.realize().ok()?;
    ctx.make_current();
    // SAFETY: a GL context is current on this thread; glGetString returns a
    // static NUL-terminated string or NULL.
    let name = unsafe {
        epoxy_glGetString.and_then(|f| {
            let p = f(GL_RENDERER);
            (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
        })
    };
    gdk::GLContext::clear_current();
    name
}

pub fn is_software(name: &str) -> bool {
    let n = name.to_lowercase();
    ["llvmpipe", "softpipe", "swrast", "software rasterizer"].iter().any(|s| n.contains(s))
}

/// Sets GSK_RENDERER before the first window is realized. `setting` is
/// "auto" or a GSK renderer name; an existing GSK_RENDERER always wins.
/// Returns what was chosen, for the log.
pub fn choose(display: &gdk::Display, setting: &str) -> Option<String> {
    if std::env::var_os("GSK_RENDERER").is_some() {
        return None;
    }
    let pick = match setting {
        "" | "auto" => match gl_renderer(display) {
            Some(n) if is_software(&n) => Some(format!("cairo (OpenGL is software: {n})")),
            Some(_) => None,
            None => Some("cairo (no OpenGL)".into()),
        },
        other => Some(other.to_string()),
    }?;
    let name = pick.split_whitespace().next().unwrap_or("cairo").to_string();
    // Single-threaded here: called during startup, before any worker thread.
    std::env::set_var("GSK_RENDERER", &name);
    Some(pick)
}

#[cfg(test)]
mod tests {
    use super::is_software;

    #[test]
    fn software_names() {
        assert!(is_software("llvmpipe (LLVM 20.1.2, 256 bits)"));
        assert!(is_software("Software Rasterizer"));
        assert!(!is_software("SVGA3D; build: RELEASE;"));
        assert!(!is_software("Mesa Intel(R) UHD Graphics 620"));
    }
}
