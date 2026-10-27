//! Compiles gpui_windows' HLSL with the real Windows d3dcompiler_47.dll (the compiler behind fxc.exe)
//! and writes the `shaders_bytes.rs` that gpui_windows' build script would have produced
//! (same flags as its build.rs: `/T {vs,ps}_4_1 /E <module>_{vertex,fragment} /O3`).
//! Usage: oats-shadergen <hlsl-dir> <out.rs>   (Windows only)
#[cfg(not(windows))]
fn main() {
    eprintln!("oats-shadergen runs on Windows only");
    std::process::exit(2);
}

#[cfg(windows)]
mod win {
    use std::ffi::{c_char, c_void, CString};
    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryA(name: *const c_char) -> *mut c_void;
        fn GetProcAddress(m: *mut c_void, name: *const c_char) -> *mut c_void;
    }
    type Blob = *mut *const [usize; 8]; // COM object: first field is the vtable pointer
    type D3DCompile = unsafe extern "system" fn(
        *const u8, usize, *const c_char, *const c_void, *const c_void, *const c_char, *const c_char,
        u32, u32, *mut Blob, *mut Blob,
    ) -> i32;
    const D3DCOMPILE_OPTIMIZATION_LEVEL3: u32 = 1 << 15;

    unsafe fn blob_bytes(b: Blob) -> Vec<u8> {
        // IUnknown: 0 QueryInterface, 1 AddRef, 2 Release; ID3DBlob: 3 GetBufferPointer, 4 GetBufferSize
        let vt = *(b as *const *const usize);
        let ptr: unsafe extern "system" fn(Blob) -> *const u8 = std::mem::transmute(*vt.add(3));
        let len: unsafe extern "system" fn(Blob) -> usize = std::mem::transmute(*vt.add(4));
        std::slice::from_raw_parts(ptr(b), len(b)).to_vec()
    }

    pub fn compile(src: &[u8], name: &str, entry: &str, target: &str) -> Result<Vec<u8>, String> {
        unsafe {
            let lib = LoadLibraryA(c"d3dcompiler_47.dll".as_ptr());
            if lib.is_null() {
                return Err("d3dcompiler_47.dll not found".into());
            }
            let f = GetProcAddress(lib, c"D3DCompile".as_ptr());
            if f.is_null() {
                return Err("no D3DCompile".into());
            }
            let f: D3DCompile = std::mem::transmute(f);
            let (n, e, t) = (CString::new(name).unwrap(), CString::new(entry).unwrap(), CString::new(target).unwrap());
            let (mut code, mut err): (Blob, Blob) = (std::ptr::null_mut(), std::ptr::null_mut());
            // include handler 1 == D3D_COMPILE_STANDARD_FILE_INCLUDE (resolves #include relative to the source file)
            let hr = f(src.as_ptr(), src.len(), n.as_ptr(), std::ptr::null(), 1 as *const c_void, e.as_ptr(), t.as_ptr(),
                D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &mut code, &mut err);
            if hr < 0 || code.is_null() {
                let m = if err.is_null() { String::new() } else { String::from_utf8_lossy(&blob_bytes(err)).into_owned() };
                return Err(format!("{entry} {target}: hr={hr:#x} {m}"));
            }
            Ok(blob_bytes(code))
        }
    }
}

#[cfg(windows)]
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (dir, out) = (std::path::PathBuf::from(&a[1]), &a[2]);
    // Exactly the module list and order of gpui_windows/build.rs, then emoji_rasterization from color_text_raster.hlsl.
    let jobs: Vec<(&str, &str)> = ["quad", "shadow", "path_rasterization", "path_sprite", "underline", "monochrome_sprite", "subpixel_sprite", "polychrome_sprite"]
        .iter().map(|m| (*m, "shaders.hlsl")).chain([("emoji_rasterization", "color_text_raster.hlsl")]).collect();
    let mut text = String::new();
    for (module, file) in jobs {
        let path = dir.join(file);
        let src = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for (stage, target, suffix) in [("vertex", "vs_4_1", "VERTEX"), ("fragment", "ps_4_1", "FRAGMENT")] {
            let entry = format!("{module}_{stage}");
            let bytes = win::compile(&src, &path.to_string_lossy(), &entry, target).unwrap_or_else(|e| panic!("{e}"));
            let body: Vec<String> = bytes.iter().map(|b| format!("0x{b:02x}")).collect();
            text += &format!("const {}_{suffix}_BYTES: &[u8] = &[{}];\n", module.to_uppercase(), body.join(", "));
            eprintln!("shadergen {entry} {target} {} bytes", bytes.len());
        }
    }
    std::fs::write(out, text).unwrap();
}
