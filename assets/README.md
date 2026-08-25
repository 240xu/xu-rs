OpenCode Termux Loader Assets

These aarch64 assets are used to adapt the official `opencode-linux-arm64`
Bun executable for Termux:

- `opencode-build.py`
- `opencode-wrapper-aarch64`
- `opencode-bunfs-shim-aarch64.so`

Source project: https://github.com/Hope2333/bun-termux-loader
Source commit: 8da3340f9235ab991161191237adb3d82cf883b4

The checked-in binaries were built and verified on Termux aarch64. They are
architecture-specific and must not be used on x86_64 or non-Termux systems.

SHA256:

48ec40fea379a7e7d96d448bfedca2a6fd18d8d46d86c999ed519a8a406af8ed  opencode-build.py
08a655771ac6c22d931507f00bfe1d0275ef7cd76bb8898f417db83093bd6a42  opencode-wrapper-aarch64
887b68a5867a2f02f3f24ac266871db41f47df717c21438dbe17dfea98a180e9  opencode-bunfs-shim-aarch64.so
