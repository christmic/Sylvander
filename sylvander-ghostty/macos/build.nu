#!/usr/bin/env nu

# Build the macOS Ghostty app using xcodebuild with a clean environment
# to avoid Nix shell interference (NIX_LDFLAGS, NIX_CFLAGS_COMPILE, etc.).

def main [
    --scheme: string = "Sylvander"     # Xcode scheme (Sylvander, Ghostty-iOS, DockTilePlugin)
    --configuration: string = "Debug"  # Build configuration (Debug, Release, ReleaseLocal)
    --action: string = "build"         # xcodebuild action (build, test, clean, etc.)
] {
    let project = ($env.FILE_PWD | path join "Sylvander.xcodeproj")
    let build_dir = ($env.FILE_PWD | path join "build")
    let repository = ($env.FILE_PWD | path join ".." ".." | path expand)
    let rust_target_dir = if "CARGO_TARGET_DIR" in $env {
        $env.CARGO_TARGET_DIR | path expand
    } else {
        $repository | path join "target"
    }

    let helper_path = if $scheme == "Sylvander" and $action != "clean" {
        if $configuration == "Debug" {
            ^cargo build --manifest-path ($repository | path join "Cargo.toml") --locked -p sylvander-tui
            ($rust_target_dir | path join "debug" "sylvander-tui")
        } else {
            let output = ($build_dir | path join "helpers" "release" "sylvander-tui")
            ^bash ($env.FILE_PWD | path join "Scripts" "build-sylvander-tui-universal.sh") $repository $output
            $output
        }
    } else {
        ""
    }

    # Skip UI tests for CLI-based invocations because it requires
    # special permissions.
    let skip_testing = if $action == "test" {
        [-skip-testing GhosttyUITests]
    } else {
        []
    }

    (^env -i
        $"HOME=($env.HOME)"
        "PATH=/usr/bin:/bin:/usr/sbin:/sbin"
        $"SYLVANDER_TUI_PATH=($helper_path)"
        xcodebuild
        -project $project
        -scheme $scheme
        -configuration $configuration
        $"SYMROOT=($build_dir)"
        ...$skip_testing
        $action)
}
