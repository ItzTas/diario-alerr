FROM --platform=$BUILDPLATFORM golang:1.27.2-alpine3.24 AS build

WORKDIR /src

COPY go.mod go.sum ./
RUN go mod download

COPY . .
ARG TARGETOS TARGETARCH
RUN CGO_ENABLED=0 GOOS=$TARGETOS GOARCH=$TARGETARCH go build \
      -trimpath \
      -ldflags "-s -w" \
      -o /out/diario-alerr .

FROM alpine:3.24

# hadolint ignore=DL3018
RUN apk add --no-cache ca-certificates poppler-utils

COPY --from=build /out/diario-alerr /usr/local/bin/diario-alerr

USER 2000:2000

EXPOSE 8080

HEALTHCHECK --interval=1m --timeout=10s --start-period=30s --retries=3 \
    CMD ["wget", "-q", "-O", "/dev/null", "http://127.0.0.1:8080/healthz"]

ENTRYPOINT ["/usr/local/bin/diario-alerr"]
