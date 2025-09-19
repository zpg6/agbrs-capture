# GBA GIF Capture Tool

Cross-platform Rust tool that captures frames from mGBA windows and creates GIFs automatically for each binary in an agbrs project.

![Example](./docs/color_spin.gif)

**Features:**

- Automatically discovers and builds all binaries in `src/bin/`
- Configurable GIF settings (FPS and duration)
- Parallel frame capture for fast execution
- Automatic mGBA window detection with retry logic
- Cross-platform support (macOS, Windows, Linux)

## Installation

Install as a global cargo command:

```bash
cargo install --path .
```

Or install from a git repository:

```bash
cargo install --git https://github.com/yourusername/gba-capture
```

## Usage

```bash
# Basic usage with defaults (10fps, 3 seconds)
gba-capture /path/to/agbrs-project

# Custom settings
gba-capture /path/to/agbrs-project --fps 15 --duration 2.5

# Get help
gba-capture --help
```

### Options

- `--fps <FPS>`: Frames per second for the output GIF (default: 10.0)
- `--duration <SECONDS>`: Duration of the GIF in seconds (default: 3.0)

This will:

1. Discover and pre-build all binaries in `src/bin/`
2. Run each binary and wait for mGBA to start
3. Capture frames and create GIF files in the `out/` folder
