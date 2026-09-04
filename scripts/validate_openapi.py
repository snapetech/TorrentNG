#!/usr/bin/env python3
"""Validate the checked-in native API contract without third-party tooling.

This is deliberately a contract sanity check, not a replacement for a full
OpenAPI validator. It catches the failures that are cheap to introduce here:
invalid JSON, duplicate operation ids, missing path parameters, and mutating
native endpoints that forget the retry/idempotency contract.
"""

from __future__ import annotations

import json
import pathlib
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC_PATH = ROOT / "docs" / "API.openapi.json"
METHODS = {"get", "put", "post", "patch", "delete", "options", "head"}
MUTATIONS = {"put", "post", "patch", "delete"}


def fail(message: str) -> None:
    raise SystemExit(f"OpenAPI validation failed: {message}")


def ref_name(ref: object) -> str | None:
    if not isinstance(ref, str):
        return None
    return ref.rsplit("/", 1)[-1]


def refs(parameters: object) -> set[str]:
    if not isinstance(parameters, list):
        return set()
    names: set[str] = set()
    for item in parameters:
        if isinstance(item, dict):
            name = ref_name(item.get("$ref"))
            if name:
                names.add(name)
    return names


def parameter_names(parameters: object, components: dict) -> set[str]:
    if not isinstance(parameters, list):
        return set()
    names: set[str] = set()
    definitions = components.get("parameters", {})
    for item in parameters:
        if not isinstance(item, dict):
            continue
        if isinstance(item.get("$ref"), str):
            definition = definitions.get(ref_name(item["$ref"]), {})
            if isinstance(definition, dict) and isinstance(definition.get("name"), str):
                names.add(definition["name"])
        elif isinstance(item.get("name"), str):
            names.add(item["name"])
    return names


def main() -> None:
    try:
        document = json.loads(SPEC_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        fail(str(exc))

    if document.get("openapi") != "3.1.0":
        fail("the document must declare OpenAPI 3.1.0")
    if not isinstance(document.get("info"), dict):
        fail("info is missing")
    paths = document.get("paths")
    if not isinstance(paths, dict) or not paths:
        fail("paths is empty")
    components = document.get("components")
    if not isinstance(components, dict):
        fail("components is missing")
    if not {"bearerAuth", "sessionCookie"} <= set(components.get("securitySchemes", {})):
        fail("both bearerAuth and sessionCookie must be declared")
    if "IdempotencyKey" not in components.get("parameters", {}):
        fail("IdempotencyKey parameter is missing")

    operation_ids: set[str] = set()
    required_paths = {
        "/health",
        "/metrics",
        "/api/v1/auth/login",
        "/api/v1/torrents",
        "/api/v1/torrents/{hash}",
        "/api/v1/events",
        "/api/v1/jobs",
        "/api/v1/storage/execute",
    }
    missing = required_paths - set(paths)
    if missing:
        fail(f"required paths missing: {sorted(missing)}")

    for path, path_item in paths.items():
        if not isinstance(path_item, dict):
            fail(f"{path} is not an object")
        path_parameters = refs(path_item.get("parameters"))
        path_parameter_names = parameter_names(path_item.get("parameters"), components)
        placeholders = {part[1:-1] for part in path.split("/") if part.startswith("{") and part.endswith("}")}
        for parameter in placeholders:
            if parameter not in path_parameter_names:
                fail(f"{path} has no declaration for path parameter {parameter}")
        for method, operation in path_item.items():
            if method == "parameters":
                continue
            if method not in METHODS:
                fail(f"{path} contains unsupported path item {method}")
            if not isinstance(operation, dict):
                fail(f"{path} {method} is not an operation object")
            operation_id = operation.get("operationId")
            if not isinstance(operation_id, str) or not operation_id:
                fail(f"{path} {method} has no operationId")
            if operation_id in operation_ids:
                fail(f"duplicate operationId {operation_id}")
            operation_ids.add(operation_id)
            if not isinstance(operation.get("responses"), dict) or not operation["responses"]:
                fail(f"{path} {method} has no responses")
            if method in MUTATIONS and not path.endswith("/auth/login") and not path.endswith("/auth/logout"):
                operation_parameters = path_parameters | refs(operation.get("parameters"))
                if "IdempotencyKey" not in operation_parameters:
                    fail(f"{path} {method} is missing the Idempotency-Key parameter")

    print(f"validated {len(paths)} paths and {len(operation_ids)} operations in {SPEC_PATH.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
