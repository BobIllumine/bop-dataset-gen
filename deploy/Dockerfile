# bop-server with the wgpu (Vulkan) and BlenderProc (Cycles) renderers, for
# Linux x86_64 hosts with an NVIDIA GPU and the NVIDIA Container Toolkit.
#   docker build -f deploy/Dockerfile -t bop-server .

FROM rust:1.97.1-trixie AS build
RUN apt-get update && apt-get install -y --no-install-recommends cmake clang \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p bop-server --features blenderproc \
    && cargo build --release --locked -p bop-cli --features blenderproc

FROM debian:trixie-slim
# Vulkan loader for wgpu; X/GL client libraries Blender links against; GLib
# for the OpenCV wheel BlenderProc imports.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl xz-utils python3 \
        libvulkan1 libegl1 libgl1 libglu1-mesa libglib2.0-0t64 \
        libxi6 libxxf86vm1 libxfixes3 libxrender1 libxkbcommon0 libsm6 libxext6 libx11-6 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=ghcr.io/astral-sh/uv:0.8.22 /uv /usr/local/bin/uv

RUN useradd --create-home --uid 10001 bop \
    && mkdir -p /opt/bop /var/lib/bop && chown bop:bop /opt/bop /var/lib/bop
USER bop
# GitPython (imported by BlenderProc) otherwise refuses to load without git.
ENV HOME=/home/bop UV_PYTHON_INSTALL_DIR=/opt/bop/python GIT_PYTHON_REFRESH=quiet
WORKDIR /opt/bop

# BlenderProc 2.8.0 from the locked worker environment. Its first run downloads
# the Blender 4.2.1 it is pinned to into ~/blender and installs its packages
# there, so do that now rather than on the first job.
COPY --chown=bop:bop tools/blenderproc-worker/pyproject.toml tools/blenderproc-worker/uv.lock blenderproc-worker/
RUN cd blenderproc-worker && uv sync --frozen \
    && printf 'import blenderproc\n' > /tmp/warm.py \
    && .venv/bin/blenderproc run /tmp/warm.py \
    && rm /tmp/warm.py
COPY --chown=bop:bop tools/blenderproc-worker/blenderproc_worker.py blenderproc-worker/

COPY --from=build /src/target/release/bop-server /src/target/release/bop-gen /opt/bop/
# Example meshes a new asset library starts with (BOP_SEED_EXAMPLES).
COPY --chown=bop:bop examples/assets/cube.stl examples/assets/hex_prism.stl \
    examples/assets/l_bracket.stl examples/assets/textured_cube.glb examples/

# The NVIDIA runtime only injects the Vulkan ICD when "graphics" is requested.
ENV NVIDIA_DRIVER_CAPABILITIES=compute,graphics,utility \
    BOP_LISTEN=0.0.0.0:8080 \
    BOP_DATA_DIR=/var/lib/bop \
    BOP_BLENDERPROC_EXECUTABLE=/opt/bop/blenderproc-worker/.venv/bin/blenderproc \
    BOP_BLENDERPROC_WORKER=/opt/bop/blenderproc-worker/blenderproc_worker.py
VOLUME /var/lib/bop
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1
ENTRYPOINT ["/opt/bop/bop-server"]
