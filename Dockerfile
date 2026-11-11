# The image is the runtime only: Alpine and the static (musl) binary,
# which is built beforehand, outside the image, so that the build uses the
# machine's own cargo cache rather than a cold one in a container:
#
#   mise run build:docker   (builds dist/organizer-linux-<arch>, then this)
#
# The binary embeds the frontend and the database schema. TARGETARCH
# (amd64 or arm64) is set by podman and buildx for the platform built for;
# `mise run build:linux` with ARCH=amd64 or arm64 builds either one, on a
# Mac too.
# An exact Alpine release, so the base changes only in a pull request
# (Dependabot's, as fix(deps): a release follows). `apk upgrade` takes the
# package fixes Alpine published since that release.
FROM alpine:3.24.4
ARG TARGETARCH
ARG BINARY=dist/organizer-linux-${TARGETARCH}

# A writable /data for the database and the files.
RUN apk upgrade --no-cache \
  && apk add --no-cache ca-certificates \
  && addgroup -S organizer && adduser -S -G organizer organizer \
  && mkdir /data && chown organizer /data
COPY --chmod=755 ${BINARY} /usr/local/bin/organizer
ENV PORT=8000 \
    DATA_DIR=/data
VOLUME /data
EXPOSE 8000

# The healthcheck: `organizer healthcheck` asks the running server's
# /api/health (on $PORT) and fails unless it answers 200.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD ["/usr/local/bin/organizer", "healthcheck"]

USER organizer
ENTRYPOINT ["/usr/local/bin/organizer"]
