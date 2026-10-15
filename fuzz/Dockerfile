# Fuzzing image: the Linux test image plus Hypothesis.
FROM git-repo-worktree-linux-test
USER root
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        procps python3-hypothesis \
    && rm -rf /var/lib/apt/lists/*
USER dev
