{ pkgs ? import <nixpkgs> { } }:
with pkgs;

pkgs.mkShell {
  buildInputs = [
    rustup
    pkg-config
    jemalloc
    rocksdb
    elfutils
    udev
    llvmPackages_15.llvm
    llvmPackages_15.clang
    llvmPackages_15.libclang
    openssl
    gnumake
    protobuf
    watchexec
    k6
    nodejs_24
    nodePackages.pnpm
    cloudflared
  ];

  shellHook = ''
    export PKG_CONFIG_PATH=/usr/lib/x86_64-linux-gnu/pkgconfig
    export JEMALLOC_OVERRIDE="${jemalloc.out}/lib/libjemalloc.a"
    export LIBCLANG_PATH="${llvmPackages_15.libclang.lib}/lib"
    export PATH="$PATH:$(pwd)/target/release:$(pwd)/target/debug"

    # FoundationDB paths for the Rust crate to find headers and libraries
    export FDB_INCLUDE_DIR="${foundationdb.dev}/include/foundationdb"
    export FDB_LIB_DIR="${foundationdb.lib}/lib"

    # For local development with sudo access, set up FoundationDB headers
    if [ -f "${foundationdb.dev}/include/foundationdb/fdb.options" ]; then
      if [ ! -e /usr/include/foundationdb/fdb.options ] && command -v sudo >/dev/null 2>&1; then
        echo "Setting up FoundationDB headers (may require sudo password)..."
        sudo mkdir -p /usr/include/foundationdb 2>/dev/null
        sudo ln -sf ${foundationdb.dev}/include/foundationdb/* /usr/include/foundationdb/ 2>/dev/null
      fi
    fi
  '';
}
