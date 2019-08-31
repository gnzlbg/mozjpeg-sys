#[allow(unused_imports)]
use std::path::{Path, PathBuf};
use std::fs;
use std::env;
use std::process;
use std::io::Write;

fn compiler(config_dir: &Path, vendor: &Path) -> cc::Build {
    let mut c = cc::Build::new();
    c.include(&config_dir);
    c.include(&vendor);
    c.pic(true);
    c.warnings(false);

    if let Ok(target_cpu) = env::var("TARGET_CPU") {
        c.flag_if_supported(&format!("-march={}", target_cpu));
    }

    if cfg!(feature = "unwinding") {
        c.flag_if_supported("-fexceptions");
    }

    c
}

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
    let root = dunce::canonicalize(root).expect("root dir");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("outdir"));
    let config_dir = out_dir.join("include");
    let vendor = root.join("vendor");

    let _ = fs::create_dir_all(&config_dir);

    println!("cargo:include={}", env::join_paths(&[&config_dir, &vendor]).expect("inc").to_str().expect("inc"));
    let mut c = compiler(&config_dir, &vendor);

    let target_pointer_width = env::var("CARGO_CFG_TARGET_POINTER_WIDTH").expect("target");

    let files = &[
        "vendor/jcapimin.c", "vendor/jcapistd.c", "vendor/jccoefct.c", "vendor/jccolor.c",
        "vendor/jcdctmgr.c", "vendor/jcext.c", "vendor/jchuff.c", "vendor/jcinit.c",
        "vendor/jcmainct.c", "vendor/jcmarker.c", "vendor/jcmaster.c", "vendor/jcomapi.c",
        "vendor/jcparam.c", "vendor/jcphuff.c", "vendor/jcprepct.c", "vendor/jcsample.c",
        "vendor/jctrans.c", "vendor/jdapimin.c", "vendor/jdapistd.c", "vendor/jdatadst.c",
        "vendor/jdatasrc.c", "vendor/jdcoefct.c", "vendor/jdcolor.c", "vendor/jddctmgr.c",
        "vendor/jdhuff.c", "vendor/jdinput.c", "vendor/jdmainct.c", "vendor/jdmarker.c",
        "vendor/jdmaster.c", "vendor/jdmerge.c", "vendor/jdphuff.c", "vendor/jdpostct.c",
        "vendor/jdsample.c", "vendor/jdtrans.c", "vendor/jerror.c", "vendor/jfdctflt.c",
        "vendor/jfdctfst.c", "vendor/jfdctint.c", "vendor/jidctflt.c", "vendor/jidctfst.c",
        "vendor/jidctint.c", "vendor/jidctred.c", "vendor/jmemmgr.c", "vendor/jmemnobs.c",
        "vendor/jquant1.c", "vendor/jquant2.c", "vendor/jutils.c",
    ];

    for file in files.iter() {
        assert!(Path::new(file).exists(), "C file is missing. Maybe you need to run `git submodule update --init`?");
        c.file(file);
    }

    let abi = if cfg!(feature = "jpeg80_abi") {
        "80"
    } else if cfg!(feature = "jpeg70_abi") {
        "70"
    } else {
        "62"
    };
    println!("cargo:lib_version={}", abi);

    let mut jconfigint_h = fs::File::create(config_dir.join("jconfigint.h")).expect("jconfint");
    write!(jconfigint_h, r#"
        #define BUILD "{timestamp}-mozjpeg-sys"
        #ifndef INLINE
            #if defined(__GNUC__)
                #define INLINE inline __attribute__((always_inline))
            #elif defined(_MSC_VER)
                #define INLINE __forceinline
            #else
                #define INLINE inline
            #endif
        #endif
        #define PACKAGE_NAME "{PACKAGE_NAME}"
        #define VERSION "{VERSION}"
        #define SIZEOF_SIZE_T {SIZEOF_SIZE_T}
        "#,
        timestamp = std::time::UNIX_EPOCH.elapsed().unwrap().as_secs(),
        PACKAGE_NAME = env::var("CARGO_PKG_NAME").expect("pkg"),
        VERSION = env::var("CARGO_PKG_VERSION").expect("pkg"),
        SIZEOF_SIZE_T = if target_pointer_width == "32" {4} else {8}
    ).expect("write");
    drop(jconfigint_h); // close the file

    let mut jconfig_h = fs::File::create(config_dir.join("jconfig.h")).expect("jconf");
    write!(jconfig_h, r#"
        #define JPEG_LIB_VERSION {JPEG_LIB_VERSION}
        #define LIBJPEG_TURBO_VERSION 0
        #define BITS_IN_JSAMPLE 8
        #define STDC_HEADERS 1
        #define HAVE_STDLIB_H 1
        #define HAVE_UNSIGNED_CHAR 1
        #define HAVE_UNSIGNED_SHORT 1
        #define MEM_SRCDST_SUPPORTED 1
        "#,
        JPEG_LIB_VERSION = abi
    ).expect("write");

    if cfg!(feature = "arith_enc") {
        jconfig_h.write_all(b"#define C_ARITH_CODING_SUPPORTED 1\n").expect("write");
        c.file("vendor/jcarith.c");
    }
    if cfg!(feature = "arith_dec") {
        jconfig_h.write_all(b"#define D_ARITH_CODING_SUPPORTED 1\n").expect("write");
        c.file("vendor/jdarith.c");
    }

    if cfg!(feature = "arith_enc") || cfg!(feature = "arith_dec") {
        c.file("vendor/jaricom.c");
    }

    if cfg!(feature = "turbojpeg_api") {
        c.file("vendor/turbojpeg.c");
        c.file("vendor/transupp.c");
        c.file("vendor/jdatadst-tj.c");
        c.file("vendor/jdatasrc-tj.c");
    }

    // cfg!(target_arch) doesn't work for cross-compiling.
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("arch");

    let nasm_needed_for_arch = match target_arch.as_str() {
        "x86_64" | "x86" => true,
        _ => false,
    };

    let with_simd = cfg!(feature = "with_simd") && (!nasm_needed_for_arch || nasm_supported());

    #[cfg(feature = "with_simd")]
    {
        if with_simd {
            c.include(vendor.join("simd"));
            jconfig_h.write_all(b"#define WITH_SIMD 1\n").unwrap();

            let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();

            match target_arch.as_str() {
                "x86_64" => {
                    c.flag_if_supported("-msse");
                    c.file("vendor/simd/x86_64/jsimd.c");
                },
                "x86" => {
                    c.flag_if_supported("-msse");
                    c.file("vendor/simd/i386/jsimd.c");
                },
                "mips" => {c.file("vendor/simd/mips/jsimd.c");},
                "powerpc" | "powerpc64" => {
                    c.flag_if_supported("-maltivec");
                    c.file("vendor/simd/powerpc/jsimd.c");
                },
                "arm" => {c.file("vendor/simd/arm/jsimd.c");},
                "aarch64" => {c.file("vendor/simd/arm64/jsimd.c");},
                _ => {},
            }
            if nasm_needed_for_arch {
                #[cfg(feature = "nasm_simd")]
                {
                    for obj in build_nasm(&root, &vendor, &out_dir, &target_arch, &target_os) {
                        c.object(obj);
                    }
                }
            } else {
                build_gas(compiler(&config_dir, &vendor), &target_arch, abi);
            };
        }
    }
    drop(jconfig_h); // close the file

    if !with_simd {
        c.file("vendor/jsimd_none.c");
    }

    c.compile(&format!("mozjpeg{}", abi));

    generate_ffi_mod();
}

fn nasm_supported() -> bool {
    if cfg!(feature = "nasm_simd") {
        match process::Command::new("nasm").arg("-v").output() {
            Err(e) => {
                println!("cargo:warning=NASM not installed. Mozjpeg's SIMD won't be enabled: {}", e);
                false
            },
            Ok(out) => {
                let ver = String::from_utf8_lossy(&out.stdout);
                if ver.contains("NASM version 0.") {
                    println!("cargo:warning=Installed NASM is outdated and useless. Mozjpeg's SIMD won't be enabled: {}", ver);
                    false
                } else {
                    true
                }
            }
        }
    } else {
        false
    }
}

#[cfg(feature = "with_simd")]
fn build_gas(mut c: cc::Build, target_arch: &str, abi: &str) {
    c.file(match target_arch {
        "arm" => "vendor/simd/arm/jsimd_neon.S",
        "aarch64" => "vendor/simd/arm64/jsimd_neon.S",
        "mips" => "vendor/simd/mips/jsimd_dspr2.S",
        _ => {panic!("The mozjpeg-sys SIMD build script is incomplete for this platform");},
    });
    c.flag("-xassembler-with-cpp");

    c.compile(&format!("mozjpegsimd{}", abi));
}

#[cfg(feature = "nasm_simd")]
fn build_nasm(root: &Path, vendor_dir: &Path, out_dir: &Path, target_arch: &str, target_os: &str) -> Vec<PathBuf> {
    let mut n = nasm_rs::Build::new();
    n.out_dir(out_dir);

    if std::env::var("PROFILE").ok().map_or(false, |s| "debug" == s) {
        n.debug(true);
    }

    n.define("PIC", None); // Rust always uses -fPIC

    match (target_os, target_arch.ends_with("64")) {
        ("windows", false) => n.define("WIN32", None),
        ("windows", true) => n.define("WIN64", None),
        ("macos", _) | ("ios", _) => n.define("MACHO", None),
        _ => n.define("ELF", None),
    };

    let arch_name = match target_arch {
        "x86" => "i386",
        "x86_64" => {
            n.define("__x86_64__", None);
            "x86_64"
        },
        _ => {panic!("The mozjpeg-sys SIMD build script is incomplete for this platform");},
    };

    // these should have had .inc extension
    let dont_compile = ["jccolext-avx2.asm", "jccolext-mmx.asm", "jccolext-sse2.asm", "jcgryext-avx2.asm",
        "jcgryext-mmx.asm", "jcgryext-sse2.asm", "jdcolext-avx2.asm", "jdcolext-mmx.asm",
        "jdcolext-sse2.asm", "jdmrgext-avx2.asm", "jdmrgext-mmx.asm", "jdmrgext-sse2.asm"];

    let simd_dir = vendor_dir.join("simd");
    let simd_arch_dir = simd_dir.join(arch_name);
    n.include(&simd_arch_dir);
    n.include(simd_dir.join("nasm"));
    n.include(vendor_dir.join("win"));
    for entry in fs::read_dir(simd_arch_dir).expect("simd subdir missing") {
        let entry = entry.unwrap();
        let path = entry.path();
        let included = path.extension().map_or(false, |e| e == "asm");
        let excluded = path.file_name().map_or(true, |f| dont_compile.iter().any(|&e| e == f));
        if included && !excluded {
            n.file(path.strip_prefix(root).unwrap_or(&path));
        }
    }
    n.compile_objects()
}

fn generate_ffi_mod() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("outdir"));
    let src_dir = env::current_dir().expect("failed to get current directory");

    eprintln!("OUT_DIR={}", out_dir.display());
    eprintln!("SRC_DIR={}", src_dir.display());
    let ffi_input = src_dir.join("src").join("lib.rs");
    println!("{}", format!("cargo:rerun-if-changed={}", ffi_input.display()));

    let mut cfg = ffi_wrapper_nounwind::Builder::new("mozjpeg", &ffi_input);
    cfg.header("jinclude.h")
        .header("jpeglib.h")
        .header("jconfig.h")
        .header("jconfigint.h")
        .skip_fn(|s| {
            match s {
                // FIXME: the Rust API of this function does not match
                // their C API, so the auto-generated wrappers fail
                // fail to compile in C. The fix would be to get the
                // crate to pass, e.g., the `ctest` tests first.
                | "jpeg_std_error"
                | "jpeg_float_add_quant_table"
                | "jpeg_calc_jpeg_dimensions"
                | "jsimd_fdct_ifast"
                | "jsimd_can_rgb_ycc"
                | "jsimd_can_fdct_ifast"
                | "jpeg_set_idct_method_selector"
                | "jpeg_read_scanlines"
                | "jpeg_has_multiple_scans"
                | "jpeg_input_complete"
                | "jpeg_c_bool_param_supported"
                | "jpeg_c_float_param_supported"
                | "jpeg_c_get_float_param"
                | "jpeg_c_int_param_supported"
                | "jpeg_c_get_int_param"
                | "jpeg_c_get_bool_param"
                | "jpeg_copy_critical_parameters"
                | "jpeg_read_raw_data"
                  => true,
                _ => false,
            }
        });

    let output = cfg.generate();
    let rust_out = out_dir.join("ffi.rs");
    let cxx_out = out_dir.join("mozjpeg_cxx_wrapper.cpp");
    std::fs::write(&rust_out, output.rust).unwrap();
    std::fs::write(&cxx_out, output.cxx).unwrap();
    drop(std::process::Command::new("rustfmt").arg(&rust_out).status());

    let mozjpeg_include = src_dir.join("vendor");
    let config_dir = out_dir.join("include");
    cc::Build::new()
        .file(cxx_out)
        .flag("-Wno-return-type-c-linkage")
        .include(mozjpeg_include)
        .include(config_dir)
        .cpp(true)
        .compile("mozjpeg_cxx_wrapper");
}
