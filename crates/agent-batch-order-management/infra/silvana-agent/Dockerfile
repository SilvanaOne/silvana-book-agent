# Sidecar-контейнер для cloud-agent.
#
# Бинарь cloud-agent предполагается уже собранным на хосте
# (см. task/updated-scenario/RUNBOOK.md, "Вариант B — сборка из исходников").
# Контекст сборки — каталог infra/silvana-agent/. Из него попадают:
#   bin/cloud-agent              — исполняемый файл, скопированный prepare.sh
#   healthcheck.sh
#   sidecar-entrypoint.sh
#
# Базовый образ Ubuntu 24.04 выбран намеренно — у cloud-agent релиз требует
# glibc >= 2.38 (см. updated-scenario/RUNBOOK.md).
FROM ubuntu:24.04

ARG AGENT_DIR=/agent
ENV AGENT_DIR=${AGENT_DIR} \
    CLOUD_AGENT_BIN=/usr/local/bin/cloud-agent \
    DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

COPY bin/cloud-agent /usr/local/bin/cloud-agent
COPY healthcheck.sh /usr/local/bin/healthcheck.sh
COPY sidecar-entrypoint.sh /usr/local/bin/sidecar-entrypoint.sh
RUN chmod +x /usr/local/bin/cloud-agent /usr/local/bin/healthcheck.sh /usr/local/bin/sidecar-entrypoint.sh

WORKDIR ${AGENT_DIR}

HEALTHCHECK --interval=30s --timeout=10s --start-period=20s --retries=3 \
  CMD ["/usr/local/bin/healthcheck.sh"]

ENTRYPOINT ["/usr/local/bin/sidecar-entrypoint.sh"]
