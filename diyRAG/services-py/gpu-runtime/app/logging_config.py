"""Structured JSON logging for the gpu-runtime sidecar (MASTER_BUILD_SPEC.md §13.1).

The Rust tier logs via `tracing` in JSON; the Python sidecars match that shape so
Loki/Grafana parse one format. A `correlation_id` (generated at the gateway) is
threaded through every log line for full request reconstruction (§13.1).
"""

from __future__ import annotations

import logging
import sys

from pythonjsonlogger import jsonlogger

# Header the Rust `api-gateway` sets on every outbound hop (§13.1).
CORRELATION_HEADER = "X-Correlation-ID"

_CONFIGURED = False


class _JsonFormatter(jsonlogger.JsonFormatter):
    """Ensure stable top-level fields (service_name, level, timestamp)."""

    def add_fields(
        self,
        log_record: dict[str, object],
        record: logging.LogRecord,
        message_dict: dict[str, object],
    ) -> None:
        super().add_fields(log_record, record, message_dict)
        log_record.setdefault("service_name", "gpu-runtime")
        log_record["level"] = record.levelname
        log_record["logger"] = record.name


def configure_json_logging(level: int = logging.INFO) -> None:
    """Install a single JSON stdout handler. Idempotent."""
    global _CONFIGURED
    if _CONFIGURED:
        return
    handler = logging.StreamHandler(sys.stdout)
    handler.setFormatter(
        _JsonFormatter(
            "%(asctime)s %(level)s %(service_name)s %(name)s %(message)s",
            rename_fields={"asctime": "timestamp"},
        )
    )
    root = logging.getLogger()
    root.handlers.clear()
    root.addHandler(handler)
    root.setLevel(level)
    # uvicorn's own loggers should also flow through the JSON handler.
    for name in ("uvicorn", "uvicorn.error", "uvicorn.access"):
        lg = logging.getLogger(name)
        lg.handlers.clear()
        lg.propagate = True
    _CONFIGURED = True
