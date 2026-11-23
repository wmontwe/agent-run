# Ubuntu's Squid on 26.04 LTS (supported until 2031). Ubuntu only publishes
# edge and beta channels for this image; edge on an LTS release is the one
# still rebuilt. It runs Squid as an unprivileged user via pebble, using the
# squid.conf agent-run mounts. Pinned by digest (the multi-arch index), so
# Dependabot can keep it up to date.
FROM docker.io/ubuntu/squid:7.2-26.04_edge@sha256:42decc7f79b4e1044f4fe9b3a855af2cbfc76652f3191694726e21058443edc9

# pebble only forwards Squid's output (the access log) with --verbose.
CMD ["--verbose"]
