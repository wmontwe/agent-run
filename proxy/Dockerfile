# Ubuntu's Squid on 26.04 LTS (supported until 2031). Ubuntu only publishes
# edge and beta channels for this image; edge on an LTS release is the one
# still rebuilt. It runs Squid as an unprivileged user via pebble, using the
# squid.conf agent-run mounts. Pinned by digest (the multi-arch index), so
# Dependabot can keep it up to date.
FROM docker.io/ubuntu/squid:7.2-26.04_edge@sha256:63f71ec4e1b60aafee88fcc13483490bab9f5b4f791c24008002b73485cf155e

# pebble only forwards Squid's output (the access log) with --verbose.
CMD ["--verbose"]
