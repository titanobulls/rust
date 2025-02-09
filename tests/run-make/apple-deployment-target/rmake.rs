//! Test codegen when setting deployment targets on Apple platforms.
//!
//! This is important since its a compatibility hazard. The linker will
//! generate load commands differently based on what minimum OS it can assume.
//!
//! See https://github.com/rust-lang/rust/pull/105123.

//@ only-apple

use run_make_support::{apple_os, cmd, run_in_tmpdir, rustc, target};

/// Run vtool to check the `minos` field in LC_BUILD_VERSION.
///
/// On lower deployment targets, LC_VERSION_MIN_MACOSX, LC_VERSION_MIN_IPHONEOS and similar
/// are used instead of LC_BUILD_VERSION - these have a `version` field, so also check that.
#[track_caller]
fn minos(file: &str, version: &str) {
    cmd("vtool")
        .arg("-show-build")
        .arg(file)
        .run()
        .assert_stdout_contains_regex(format!("(minos|version) {version}"));
}

fn main() {
    // These versions should generally be higher than the default versions
    let (env_var, example_version, higher_example_version) = match apple_os() {
        "macos" => ("MACOSX_DEPLOYMENT_TARGET", "12.0", "13.0"),
        // armv7s-apple-ios and i386-apple-ios only supports iOS 10.0
        "ios" if target() == "armv7s-apple-ios" || target() == "i386-apple-ios" => {
            ("IPHONEOS_DEPLOYMENT_TARGET", "10.0", "10.0")
        }
        "ios" => ("IPHONEOS_DEPLOYMENT_TARGET", "15.0", "16.0"),
        "watchos" => ("WATCHOS_DEPLOYMENT_TARGET", "7.0", "9.0"),
        "tvos" => ("TVOS_DEPLOYMENT_TARGET", "14.0", "15.0"),
        "visionos" => ("XROS_DEPLOYMENT_TARGET", "1.1", "1.2"),
        _ => unreachable!(),
    };
    let default_version =
        rustc().target(target()).env_remove(env_var).print("deployment-target").run().stdout_utf8();
    let default_version = default_version.strip_prefix("deployment_target=").unwrap().trim();

    // Test that version makes it to the object file.
    run_in_tmpdir(|| {
        let rustc = || {
            let mut rustc = rustc();
            rustc.target(target());
            rustc.crate_type("lib");
            rustc.emit("obj");
            rustc.input("foo.rs");
            rustc.output("foo.o");
            rustc
        };

        rustc().env(env_var, example_version).run();
        minos("foo.o", example_version);

        // FIXME(madsmtm): Doesn't work on Mac Catalyst and the simulator.
        if !target().contains("macabi") && !target().contains("sim") {
            rustc().env_remove(env_var).run();
            minos("foo.o", default_version);
        }
    });

    // Test that version makes it to the linker when linking dylibs.
    run_in_tmpdir(|| {
        // Certain watchOS targets don't support dynamic linking, so we disable the test on those.
        if apple_os() == "watchos" {
            return;
        }

        let rustc = || {
            let mut rustc = rustc();
            rustc.target(target());
            rustc.crate_type("dylib");
            rustc.input("foo.rs");
            rustc.output("libfoo.dylib");
            rustc
        };

        rustc().env(env_var, example_version).run();
        minos("libfoo.dylib", example_version);

        rustc().env_remove(env_var).run();
        minos("libfoo.dylib", default_version);

        // Test with ld64 instead

        rustc().arg("-Clinker-flavor=ld").env(env_var, example_version).run();
        minos("libfoo.dylib", example_version);

        rustc().arg("-Clinker-flavor=ld").env_remove(env_var).run();
        minos("libfoo.dylib", default_version);
    });

    // Test that version makes it to the linker when linking executables.
    run_in_tmpdir(|| {
        let rustc = || {
            let mut rustc = rustc();
            rustc.target(target());
            rustc.crate_type("bin");
            rustc.input("foo.rs");
            rustc.output("foo");
            rustc
        };

        // FIXME(madsmtm): Doesn't work on watchOS for some reason?
        if !target().contains("watchos") {
            rustc().env(env_var, example_version).run();
            minos("foo", example_version);

            rustc().env_remove(env_var).run();
            minos("foo", default_version);
        }

        // Test with ld64 instead

        rustc().arg("-Clinker-flavor=ld").env(env_var, example_version).run();
        minos("foo", example_version);

        rustc().arg("-Clinker-flavor=ld").env_remove(env_var).run();
        minos("foo", default_version);
    });

    // Test that changing the deployment target busts the incremental cache.
    run_in_tmpdir(|| {
        let rustc = || {
            let mut rustc = rustc();
            rustc.target(target());
            rustc.incremental("incremental");
            rustc.crate_type("lib");
            rustc.emit("obj");
            rustc.input("foo.rs");
            rustc.output("foo.o");
            rustc
        };

        // FIXME(madsmtm): Incremental cache is not yet busted
        // https://github.com/rust-lang/rust/issues/118204
        let higher_example_version = example_version;
        let default_version = example_version;

        rustc().env(env_var, example_version).run();
        minos("foo.o", example_version);

        rustc().env(env_var, higher_example_version).run();
        minos("foo.o", higher_example_version);

        // FIXME(madsmtm): Doesn't work on Mac Catalyst and the simulator.
        if !target().contains("macabi") && !target().contains("sim") {
            rustc().env_remove(env_var).run();
            minos("foo.o", default_version);
        }
    });
}
