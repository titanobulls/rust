use std::borrow::Cow;
use std::env;
use std::num::ParseIntError;

use crate::spec::{
    add_link_args, add_link_args_iter, cvs, Cc, DebuginfoKind, FramePointer, LinkArgs,
    LinkerFlavor, Lld, SplitDebuginfo, StackProbeType, StaticCow, Target, TargetOptions,
};

#[cfg(test)]
mod tests;

use Arch::*;
#[allow(non_camel_case_types)]
#[derive(Copy, Clone, PartialEq)]
pub(crate) enum Arch {
    Armv7k,
    Armv7s,
    Arm64,
    Arm64e,
    Arm64_32,
    I386,
    I686,
    X86_64,
    X86_64h,
}

impl Arch {
    fn target_name(self) -> &'static str {
        match self {
            Armv7k => "armv7k",
            Armv7s => "armv7s",
            Arm64 => "arm64",
            Arm64e => "arm64e",
            Arm64_32 => "arm64_32",
            I386 => "i386",
            I686 => "i686",
            X86_64 => "x86_64",
            X86_64h => "x86_64h",
        }
    }

    /// The architecture name to forward to the linker.
    fn ld_arch(self) -> &'static str {
        // Supported architecture names can be found in the source:
        // https://github.com/apple-oss-distributions/ld64/blob/ld64-951.9/src/abstraction/MachOFileAbstraction.hpp#L578-L648
        match self {
            Armv7k => "armv7k",
            Armv7s => "armv7s",
            Arm64 => "arm64",
            Arm64e => "arm64e",
            Arm64_32 => "arm64_32",
            // ld64 doesn't understand i686, so fall back to i386 instead
            //
            // Same story when linking with cc, since that ends up invoking ld64.
            I386 | I686 => "i386",
            X86_64 => "x86_64",
            X86_64h => "x86_64h",
        }
    }

    pub(crate) fn target_arch(self) -> Cow<'static, str> {
        Cow::Borrowed(match self {
            Armv7k | Armv7s => "arm",
            Arm64 | Arm64e | Arm64_32 => "aarch64",
            I386 | I686 => "x86",
            X86_64 | X86_64h => "x86_64",
        })
    }

    fn target_cpu(self, abi: TargetAbi) -> &'static str {
        match self {
            Armv7k => "cortex-a8",
            Armv7s => "swift", // iOS 10 is only supported on iPhone 5 or higher.
            Arm64 => match abi {
                TargetAbi::Normal => "apple-a7",
                TargetAbi::Simulator => "apple-a12",
                TargetAbi::MacCatalyst => "apple-a12",
            },
            Arm64e => "apple-a12",
            Arm64_32 => "apple-s4",
            // Only macOS 10.12+ is supported, which means
            // all x86_64/x86 CPUs must be running at least penryn
            // https://github.com/llvm/llvm-project/blob/01f924d0e37a5deae51df0d77e10a15b63aa0c0f/clang/lib/Driver/ToolChains/Arch/X86.cpp#L79-L82
            I386 | I686 => "penryn",
            X86_64 => "penryn",
            // Note: `core-avx2` is slightly more advanced than `x86_64h`, see
            // comments (and disabled features) in `x86_64h_apple_darwin` for
            // details. It is a higher baseline then `penryn` however.
            X86_64h => "core-avx2",
        }
    }

    fn stack_probes(self) -> StackProbeType {
        match self {
            Armv7k | Armv7s => StackProbeType::None,
            Arm64 | Arm64e | Arm64_32 | I386 | I686 | X86_64 | X86_64h => StackProbeType::Inline,
        }
    }
}

#[derive(Copy, Clone, PartialEq)]
pub(crate) enum TargetAbi {
    Normal,
    Simulator,
    MacCatalyst,
}

impl TargetAbi {
    fn target_abi(self) -> &'static str {
        match self {
            Self::Normal => "",
            Self::MacCatalyst => "macabi",
            Self::Simulator => "sim",
        }
    }
}

fn pre_link_args(os: &'static str, arch: Arch, abi: TargetAbi) -> LinkArgs {
    // From the man page for ld64 (`man ld`):
    // > The linker accepts universal (multiple-architecture) input files,
    // > but always creates a "thin" (single-architecture), standard Mach-O
    // > output file. The architecture for the output file is specified using
    // > the -arch option.
    //
    // The linker has heuristics to determine the desired architecture, but to
    // be safe, and to avoid a warning, we set the architecture explicitly.
    let mut args =
        TargetOptions::link_args(LinkerFlavor::Darwin(Cc::No, Lld::No), &["-arch", arch.ld_arch()]);

    // From the man page for ld64 (`man ld`):
    // > This is set to indicate the platform, oldest supported version of
    // > that platform that output is to be used on, and the SDK that the
    // > output was built against. platform [...] may be one of the following
    // > strings:
    // > - macos
    // > - ios
    // > - tvos
    // > - watchos
    // > - bridgeos
    // > - visionos
    // > - xros
    // > - mac-catalyst
    // > - ios-simulator
    // > - tvos-simulator
    // > - watchos-simulator
    // > - visionos-simulator
    // > - xros-simulator
    // > - driverkit
    //
    // Like with `-arch`, the linker can figure out the platform versions
    // itself from the binaries being linked, but to be safe, we specify the
    // desired versions here explicitly.
    let platform_name: StaticCow<str> = match abi {
        TargetAbi::Normal => os.into(),
        TargetAbi::Simulator => format!("{os}-simulator").into(),
        TargetAbi::MacCatalyst => "mac-catalyst".into(),
    };
    let min_version: StaticCow<str> = {
        let (major, minor, patch) = deployment_target(os, arch, abi);
        format!("{major}.{minor}.{patch}").into()
    };
    // Lie about the SDK version, we don't know it here
    let sdk_version = min_version.clone();
    add_link_args_iter(
        &mut args,
        LinkerFlavor::Darwin(Cc::No, Lld::No),
        ["-platform_version".into(), platform_name, min_version, sdk_version].into_iter(),
    );

    // We need to communicate four things to the C compiler to be able to link:
    // - The architecture.
    // - The operating system (and that it's an Apple platform).
    // - The deployment target.
    // - The environment / ABI.
    //
    // We'd like to use `-target` everywhere, since that can uniquely
    // communicate all of these, but that doesn't work on GCC, and since we
    // don't know whether the `cc` compiler is Clang, GCC, or something else,
    // we fall back to other options that also work on GCC when compiling for
    // macOS.
    //
    // Targets other than macOS are ill-supported by GCC (it doesn't even
    // support e.g. `-miphoneos-version-min`), so in those cases we can fairly
    // safely use `-target`. See also the following, where it is made explicit
    // that the recommendation by LLVM developers is to use `-target`:
    // <https://github.com/llvm/llvm-project/issues/88271>
    if os == "macos" {
        // `-arch` communicates the architecture.
        //
        // CC forwards the `-arch` to the linker, so we use the same value
        // here intentionally.
        add_link_args(
            &mut args,
            LinkerFlavor::Darwin(Cc::Yes, Lld::No),
            &["-arch", arch.ld_arch()],
        );
        // The presence of `-mmacosx-version-min` makes CC default to macOS,
        // and it sets the deployment target.
        let (major, minor, patch) = deployment_target(os, arch, abi);
        let opt = format!("-mmacosx-version-min={major}.{minor}.{patch}").into();
        add_link_args_iter(&mut args, LinkerFlavor::Darwin(Cc::Yes, Lld::No), [opt].into_iter());
        // macOS has no environment, so with these two, we've told CC all the
        // desired parameters.
        //
        // We avoid `-m32`/`-m64`, as this is already encoded by `-arch`.
    } else {
        add_link_args_iter(
            &mut args,
            LinkerFlavor::Darwin(Cc::Yes, Lld::No),
            ["-target".into(), llvm_target(os, arch, abi)].into_iter(),
        );
    }

    args
}

/// Get the base target options, LLVM target and `target_arch` from the three
/// things that uniquely identify Rust's Apple targets: The OS, the
/// architecture, and the ABI.
pub(crate) fn base(
    os: &'static str,
    arch: Arch,
    abi: TargetAbi,
) -> (TargetOptions, StaticCow<str>, StaticCow<str>) {
    let opts = TargetOptions {
        abi: abi.target_abi().into(),
        os: os.into(),
        cpu: arch.target_cpu(abi).into(),
        link_env_remove: link_env_remove(os),
        vendor: "apple".into(),
        linker_flavor: LinkerFlavor::Darwin(Cc::Yes, Lld::No),
        // macOS has -dead_strip, which doesn't rely on function_sections
        function_sections: false,
        dynamic_linking: true,
        pre_link_args: pre_link_args(os, arch, abi),
        families: cvs!["unix"],
        is_like_osx: true,
        // LLVM notes that macOS 10.11+ and iOS 9+ default
        // to v4, so we do the same.
        // https://github.com/llvm/llvm-project/blob/378778a0d10c2f8d5df8ceff81f95b6002984a4b/clang/lib/Driver/ToolChains/Darwin.cpp#L1203
        default_dwarf_version: 4,
        frame_pointer: FramePointer::Always,
        has_rpath: true,
        dll_suffix: ".dylib".into(),
        archive_format: "darwin".into(),
        // Thread locals became available with iOS 8 and macOS 10.7,
        // and both are far below our minimum.
        has_thread_local: true,
        abi_return_struct_as_int: true,
        emit_debug_gdb_scripts: false,
        eh_frame_header: false,
        stack_probes: arch.stack_probes(),

        debuginfo_kind: DebuginfoKind::DwarfDsym,
        // The historical default for macOS targets is to run `dsymutil` which
        // generates a packed version of debuginfo split from the main file.
        split_debuginfo: SplitDebuginfo::Packed,
        supported_split_debuginfo: Cow::Borrowed(&[
            SplitDebuginfo::Packed,
            SplitDebuginfo::Unpacked,
            SplitDebuginfo::Off,
        ]),

        // This environment variable is pretty magical but is intended for
        // producing deterministic builds. This was first discovered to be used
        // by the `ar` tool as a way to control whether or not mtime entries in
        // the archive headers were set to zero or not. It appears that
        // eventually the linker got updated to do the same thing and now reads
        // this environment variable too in recent versions.
        //
        // For some more info see the commentary on #47086
        link_env: Cow::Borrowed(&[(Cow::Borrowed("ZERO_AR_DATE"), Cow::Borrowed("1"))]),

        ..Default::default()
    };
    (opts, llvm_target(os, arch, abi), arch.target_arch())
}

pub fn sdk_version(platform: u32) -> Option<(u16, u8)> {
    // NOTE: These values are from an arbitrary point in time but shouldn't make it into the final
    // binary since the final link command will have the current SDK version passed to it.
    match platform {
        object::macho::PLATFORM_MACOS => Some((13, 1)),
        object::macho::PLATFORM_IOS
        | object::macho::PLATFORM_IOSSIMULATOR
        | object::macho::PLATFORM_TVOS
        | object::macho::PLATFORM_TVOSSIMULATOR
        | object::macho::PLATFORM_MACCATALYST => Some((16, 2)),
        object::macho::PLATFORM_WATCHOS | object::macho::PLATFORM_WATCHOSSIMULATOR => Some((9, 1)),
        // FIXME: Upgrade to `object-rs` 0.33+ implementation with visionOS platform definition
        11 | 12 => Some((1, 0)),
        _ => None,
    }
}

pub fn platform(target: &Target) -> Option<u32> {
    Some(match (&*target.os, &*target.abi) {
        ("macos", _) => object::macho::PLATFORM_MACOS,
        ("ios", "macabi") => object::macho::PLATFORM_MACCATALYST,
        ("ios", "sim") => object::macho::PLATFORM_IOSSIMULATOR,
        ("ios", _) => object::macho::PLATFORM_IOS,
        ("watchos", "sim") => object::macho::PLATFORM_WATCHOSSIMULATOR,
        ("watchos", _) => object::macho::PLATFORM_WATCHOS,
        ("tvos", "sim") => object::macho::PLATFORM_TVOSSIMULATOR,
        ("tvos", _) => object::macho::PLATFORM_TVOS,
        // FIXME: Upgrade to `object-rs` 0.33+ implementation with visionOS platform definition
        ("visionos", "sim") => 12,
        ("visionos", _) => 11,
        _ => return None,
    })
}

/// Hack for calling `deployment_target` outside of this module.
pub fn deployment_target_for_target(target: &Target) -> (u16, u8, u8) {
    let arch = if target.llvm_target.starts_with("arm64e") {
        Arch::Arm64e
    } else if target.arch == "aarch64" {
        Arch::Arm64
    } else {
        // Dummy architecture, only used by `deployment_target` anyhow
        Arch::X86_64
    };
    let abi = match &*target.abi {
        "macabi" => TargetAbi::MacCatalyst,
        "sim" => TargetAbi::Simulator,
        "" => TargetAbi::Normal,
        abi => unreachable!("invalid abi '{abi}' for Apple target"),
    };
    deployment_target(&target.os, arch, abi)
}

/// Get the deployment target based on the standard environment variables, or
/// fall back to a sane default.
fn deployment_target(os: &str, arch: Arch, abi: TargetAbi) -> (u16, u8, u8) {
    // When bumping a version in here, remember to update the platform-support
    // docs too.
    //
    // NOTE: If you are looking for the default deployment target, prefer
    // `rustc --print deployment-target`, as the default here may change in
    // future `rustc` versions.

    // Minimum operating system versions currently supported by `rustc`.
    let os_min = match os {
        "macos" => (10, 12, 0),
        "ios" => (10, 0, 0),
        "tvos" => (10, 0, 0),
        "watchos" => (5, 0, 0),
        "visionos" => (1, 0, 0),
        _ => unreachable!("tried to get deployment target for non-Apple platform"),
    };

    // On certain targets it makes sense to raise the minimum OS version.
    let min = match (os, arch, abi) {
        // Use 11.0 on Aarch64 as that's the earliest version with M1 support.
        ("macos", Arch::Arm64 | Arch::Arm64e, _) => (11, 0, 0),
        ("ios", Arch::Arm64e, _) => (14, 0, 0),
        // Mac Catalyst defaults to 13.1 in Clang.
        ("ios", _, TargetAbi::MacCatalyst) => (13, 1, 0),
        _ => os_min,
    };

    // The environment variable used to fetch the deployment target.
    let env_var = match os {
        "macos" => "MACOSX_DEPLOYMENT_TARGET",
        "ios" => "IPHONEOS_DEPLOYMENT_TARGET",
        "watchos" => "WATCHOS_DEPLOYMENT_TARGET",
        "tvos" => "TVOS_DEPLOYMENT_TARGET",
        "visionos" => "XROS_DEPLOYMENT_TARGET",
        _ => unreachable!("tried to get deployment target env var for non-Apple platform"),
    };

    if let Ok(deployment_target) = env::var(env_var) {
        match parse_version(&deployment_target) {
            // It is common that the deployment target is set too low, e.g. on
            // macOS Aarch64 to also target older x86_64, the user may set a
            // lower deployment target than supported.
            //
            // To avoid such issues, we silently raise the deployment target
            // here.
            // FIXME: We want to show a warning when `version < os_min`.
            Ok(version) => version.max(min),
            // FIXME: Report erroneous environment variable to user.
            Err(_) => min,
        }
    } else {
        min
    }
}

/// Generate the target triple that we need to pass to LLVM and/or Clang.
fn llvm_target(os: &str, arch: Arch, abi: TargetAbi) -> StaticCow<str> {
    // The target triple depends on the deployment target, and is required to
    // enable features such as cross-language LTO, and for picking the right
    // Mach-O commands.
    //
    // Certain optimizations also depend on the deployment target.
    let (major, minor, patch) = deployment_target(os, arch, abi);
    let arch = arch.target_name();
    // Convert to the "canonical" OS name used by LLVM:
    // https://github.com/llvm/llvm-project/blob/llvmorg-18.1.8/llvm/lib/TargetParser/Triple.cpp#L236-L282
    let os = match os {
        "macos" => "macosx",
        "ios" => "ios",
        "watchos" => "watchos",
        "tvos" => "tvos",
        "visionos" => "xros",
        _ => unreachable!("tried to get LLVM target OS for non-Apple platform"),
    };
    let environment = match abi {
        TargetAbi::Normal => "",
        TargetAbi::MacCatalyst => "-macabi",
        TargetAbi::Simulator => "-simulator",
    };
    format!("{arch}-apple-{os}{major}.{minor}.{patch}{environment}").into()
}

fn link_env_remove(os: &'static str) -> StaticCow<[StaticCow<str>]> {
    // Apple platforms only officially support macOS as a host for any compilation.
    //
    // If building for macOS, we go ahead and remove any erroneous environment state
    // that's only applicable to cross-OS compilation. Always leave anything for the
    // host OS alone though.
    if os == "macos" {
        let mut env_remove = Vec::with_capacity(2);
        // Remove the `SDKROOT` environment variable if it's clearly set for the wrong platform, which
        // may occur when we're linking a custom build script while targeting iOS for example.
        if let Ok(sdkroot) = env::var("SDKROOT") {
            if sdkroot.contains("iPhoneOS.platform")
                || sdkroot.contains("iPhoneSimulator.platform")
                || sdkroot.contains("AppleTVOS.platform")
                || sdkroot.contains("AppleTVSimulator.platform")
                || sdkroot.contains("WatchOS.platform")
                || sdkroot.contains("WatchSimulator.platform")
                || sdkroot.contains("XROS.platform")
                || sdkroot.contains("XRSimulator.platform")
            {
                env_remove.push("SDKROOT".into())
            }
        }
        // Additionally, `IPHONEOS_DEPLOYMENT_TARGET` must not be set when using the Xcode linker at
        // "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/ld",
        // although this is apparently ignored when using the linker at "/usr/bin/ld".
        env_remove.push("IPHONEOS_DEPLOYMENT_TARGET".into());
        env_remove.push("TVOS_DEPLOYMENT_TARGET".into());
        env_remove.push("XROS_DEPLOYMENT_TARGET".into());
        env_remove.into()
    } else {
        // Otherwise if cross-compiling for a different OS/SDK (including Mac Catalyst), remove any part
        // of the linking environment that's wrong and reversed.
        cvs!["MACOSX_DEPLOYMENT_TARGET"]
    }
}

/// Parse an OS version triple (SDK version or deployment target).
///
/// The size of the returned numbers here are limited by Mach-O's
/// `LC_BUILD_VERSION`.
fn parse_version(version: &str) -> Result<(u16, u8, u8), ParseIntError> {
    if let Some((major, minor)) = version.split_once('.') {
        let major = major.parse()?;
        if let Some((minor, patch)) = minor.split_once('.') {
            Ok((major, minor.parse()?, patch.parse()?))
        } else {
            Ok((major, minor.parse()?, 0))
        }
    } else {
        Ok((version.parse()?, 0, 0))
    }
}
