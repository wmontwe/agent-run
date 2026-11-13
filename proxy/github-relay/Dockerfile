# Pinned by digest (the multi-arch index), so Dependabot can keep it up to date.
FROM docker.io/nginxinc/nginx-unprivileged:1.31.7-alpine@sha256:b5047b9b76845eac124358373c6852377c52e4b05834a47647e955dd91cb8f1e
COPY nginx.conf /etc/nginx/nginx.conf
COPY start.sh /etc/nginx/start.sh
# The repository list is a folder agent-run mounts at /etc/nginx/agent-run and changes
# live (SIGHUP).
ENTRYPOINT ["/bin/sh", "/etc/nginx/start.sh"]
