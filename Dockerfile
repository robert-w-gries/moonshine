# syntax=docker/dockerfile:1
#
# Development image for moonshine — https://github.com/hgaiser/moonshine
#
# Mirrors CI (.github/workflows/ci.yaml runs on ubuntu-24.04, pinned as the
# release-binary glibc baseline), so `cargo test/clippy/fmt/doc` behave the
# same locally as in CI. Also carries Mesa's Vulkan/EGL userspace so the
# server itself can run with a passthrough GPU (/dev/dri, AMD/Intel), or with
# an NVIDIA GPU via the NVIDIA Container Toolkit (driver userspace injected).
#
# Normally built via `docker compose build` (which passes UID/GID).
# Manual:
#   docker build --build-arg UID=$(id -u) --build-arg GID=$(id -g) -t moonshine-dev .

FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

# Build dependencies: keep in sync with
# .github/actions/install-dependencies/action.yml.
# Runtime additions: Vulkan loader + Mesa drivers + EGL let the container use
# a passthrough GPU; git + ca-certificates are needed for cargo's git
# dependencies (ash, inputtino, smithay, pixelforge); vulkan-tools helps
# debug GPU passthrough (vulkaninfo).
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        clang \
        cmake \
        libc++-dev \
        libc++abi-dev \
        libclang-dev \
        libdrm-dev \
        libevdev-dev \
        libgbm-dev \
        libopus-dev \
        libwayland-dev \
        libxkbcommon-dev \
        patchelf \
        pkg-config \
        # --- runtime / GPU passthrough ---
        libvulkan1 \
        mesa-vulkan-drivers \
        libegl1 \
        libegl-mesa0 \
        vulkan-tools \
        # --- runtime / X11 apps (Steam) via XWayland ---
        # Pulls xserver-common -> xkb-data, which XWayland needs to compile
        # its initial keymap (fatal "XKB: Failed to compile keymap" otherwise).
        xwayland \
        # --- tooling ---
        ca-certificates \
        curl \
        git \
    && rm -rf /var/lib/apt/lists/*

# Unprivileged user with the host's UID/GID: bind-mounted files stay owned by
# the host user (no root-owned Cargo.lock or config). ubuntu:24.04 ships a
# default 'ubuntu' user at 1000:1000 — reclaim the IDs if they collide.
ARG UID=1000
ARG GID=1000
RUN set -eux; \
    if getent passwd "$UID" >/dev/null; then userdel --remove "$(getent passwd "$UID" | cut -d: -f1)"; fi; \
    if getent group "$GID" >/dev/null; then groupdel "$(getent group "$GID" | cut -d: -f1)"; fi; \
    groupadd --gid "$GID" dev; \
    useradd --uid "$UID" --gid "$GID" --create-home --shell /bin/bash dev

# X11 socket directory for XWayland. On a normal system this is created by
# systemd-tmpfiles (/usr/lib/tmpfiles.d/x11.conf), which never runs in a
# container. Smithay binds /tmp/.X11-unix/X<N> when spawning XWayland and
# aborts with "Could not find a free socket for the XServer" if it is missing.
RUN install -d -m 1777 /tmp/.X11-unix

# NVIDIA's GBM backend (nvidia-drm_gbm.so) is injected by the NVIDIA Container
# Toolkit at the *host's* path — /usr/lib/gbm on Arch/Fedora-style hosts —
# which Ubuntu's libgbm never searches. Without it Mesa falls back to
# kms_swrast and GBM allocation fails with EACCES. Harmless on AMD/Intel.
ENV GBM_BACKENDS_PATH=/usr/lib/x86_64-linux-gnu/gbm:/usr/lib/gbm

USER dev
WORKDIR /home/dev

# Rust toolchain — matches CI's dtolnay/rust-toolchain@stable. Pin by
# overriding the build arg, e.g. --build-arg RUST_TOOLCHAIN=1.85.0.
ARG RUST_TOOLCHAIN=stable
ENV RUSTUP_HOME=/home/dev/.rustup \
    CARGO_HOME=/home/dev/.cargo \
    PATH=/home/dev/.cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain "${RUST_TOOLCHAIN}" --profile minimal \
    && rustup component add clippy rustfmt

# Build artifacts live outside the bind-mounted source tree (named volume).
# Pre-create the cache dirs so named volumes inherit dev ownership.
ENV CARGO_TARGET_DIR=/home/dev/target
RUN mkdir -p /home/dev/target /home/dev/.cargo/registry /home/dev/.cargo/git

WORKDIR /home/dev/moonshine
CMD ["sleep", "infinity"]
