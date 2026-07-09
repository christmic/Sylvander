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
    const run_tests = b.addRunArtifact(tests);
    const test_step = b.step("test", "Run sylvander module tests");
    test_step.dependOn(&run_tests.step);

    // Alias "check" to the same test compile (zig 0.16 doesn't expose a
    // standalone type-check flag; addTest does both analyse + codegen).
    const check_step = b.step("check", "Type-check the sylvander module");
    check_step.dependOn(&tests.step);
}