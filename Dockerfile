# syntax=docker/dockerfile:1
# QuantDesk: one image with qd-server, the qd CLI, the built frontend and the
# data configuration. Paper trading by default; live orders are not compiled
# in unless FEATURES="qd-server/live-orders" is passed, and even then need every
# INV-14 condition at runtime.

# ---- frontend ----
FROM node:22-bookworm-slim AS frontend
WORKDIR /src/frontend
COPY frontend/package.json frontend/package-lock.json ./
# Behind a TLS-intercepting proxy, pass its CA as a build secret
# (--secret id=ca_bundle,src=...) and HTTPS_PROXY as a build arg; neither
# is kept in the image.
RUN --mount=type=secret,id=ca_bundle,required=false \
    if [ -f /run/secrets/ca_bundle ]; then export NODE_EXTRA_CA_CERTS=/run/secrets/ca_bundle; fi; \
    npm ci
COPY frontend/ ./
RUN npm run build

# ---- backend ----
FROM rust:1.94-bookworm AS backend
ARG FEATURES=""
WORKDIR /src/backend
COPY backend/ ./
RUN --mount=type=secret,id=ca_bundle,required=false \
    if [ -f /run/secrets/ca_bundle ]; then export CARGO_HTTP_CAINFO=/run/secrets/ca_bundle; fi; \
    cargo build --release --locked -p qd-server -p qd-cli ${FEATURES:+--features "$FEATURES"} \
    && mkdir -p /out/state

# ---- runtime ----
# No package manager step: CA certificates come from the build stage, the
# healthcheck is built into qd-server, and the user is a bare non-root UID.
FROM debian:bookworm-slim
COPY --from=backend /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=backend /src/backend/target/release/qd-server /usr/local/bin/qd-server
COPY --from=backend /src/backend/target/release/qd /usr/local/bin/qd
COPY --from=frontend /src/frontend/dist /usr/share/quantdesk/frontend
COPY backend/config/risk.toml backend/config/validation.toml backend/config/review.toml /etc/quantdesk/
COPY backend/config/costs /etc/quantdesk/costs
COPY backend/config/calendars /etc/quantdesk/calendars
COPY deploy/quantdesk.toml /etc/quantdesk/quantdesk.toml
# State directory for the generated secrets master key; a named volume
# mounted here inherits this ownership.
COPY --from=backend --chown=10001:10001 /out/state /var/lib/quantdesk
USER 10001:10001
ENV QD_CONFIG=/etc/quantdesk/quantdesk.toml
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD ["/usr/local/bin/qd-server", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/qd-server"]
