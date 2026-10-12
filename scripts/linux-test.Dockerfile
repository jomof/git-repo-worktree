# Linux test image for git-repo-worktree: FUSE overlays, real repo, non-root user.
FROM ubuntu:24.04

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl fuse-overlayfs fuse3 git python3 \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL https://storage.googleapis.com/git-repo-downloads/repo -o /usr/local/bin/repo \
    && chmod 755 /usr/local/bin/repo

RUN useradd --create-home --shell /bin/bash dev && mkdir /work && chown dev /work
USER dev
ENV PATH=/home/dev/.cargo/bin:$PATH
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable \
    && git config --global user.name Test \
    && git config --global user.email test@example.com \
    && git config --global init.defaultBranch main
WORKDIR /work
