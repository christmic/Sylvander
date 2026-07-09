const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    // Sylvander module — the public surface that Ghostty and tests import.
    const sylvander_mod = b.addModule("sylvander", .{
        .root_source_file = b.path("mod.zig"),
        .target = target,
        .optimize = optimize,
    });

    // Standalone test binary so we can verify the module compiles
    // without needing to build the entire Ghostty executable.
    const tests = b.addTest(.{
        .root_module = sylvander_mod,
    });
    // The test runner pulls in std lib machinery that needs the host
    // libc. On macOS that is libSystem; without linkLibC() the linker
    // errors out with undefined symbols like _abort, _bzero,
    // _realpath$DARWIN_EXTSN, _dispatch_queue_create etc.
    // See ziglang/zig#31658 — same root cause as the CLT link bug
    // documented in SYNCUP.md.
    tests.linkLibC();
    const run_tests = b.addRunArtifact(tests);
    const test_step = b.step("test", "Run sylvander module tests");
    test_step.dependOn(&run_tests.step);

    // Alias "check" to the same test compile.
    const check_step = b.step("check", "Type-check the sylvander module");
    check_step.dependOn(&tests.step);
}