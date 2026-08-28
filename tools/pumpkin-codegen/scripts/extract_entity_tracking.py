#!/usr/bin/env python3
"""Extract Minecraft entity tracking policy into Pumpkin's checked-in asset."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path


DECLARATION = re.compile(
    r"public\s+static\s+final\s+EntityType<[^;=]+>\s+([A-Z0-9_]+)\s*=\s*register\("
)
TRACKING_RANGE = re.compile(r"\.clientTrackingRange\(\s*(\d+)\s*\)")
UPDATE_INTERVAL = re.compile(r"\.updateInterval\(\s*([^)]+?)\s*\)")
NO_DELTA_TYPES = {
    "bat",
    "end_crystal",
    "evoker_fangs",
    "glow_item_frame",
    "item_frame",
    "leash_knot",
    "llama_spit",
    "painting",
    "player",
    "wither",
}


def registration_expression(source: str, start: int) -> str:
    depth = 1
    in_string = False
    escaped = False
    index = start
    while index < len(source):
        char = source[index]
        if in_string:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
        elif char == '"':
            in_string = True
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return source[start:index]
        index += 1
    raise ValueError("unterminated EntityType registration")


def parse_interval(raw: str) -> int:
    if raw == "Integer.MAX_VALUE":
        return 2_147_483_647
    if not raw.isdecimal():
        raise ValueError(f"unsupported update interval expression: {raw}")
    return int(raw)


def extract(source: str) -> dict[str, dict[str, int | bool]]:
    policies: dict[str, dict[str, int | bool]] = {}
    for declaration in DECLARATION.finditer(source):
        name = declaration.group(1).lower()
        expression = registration_expression(source, declaration.end())
        ranges = TRACKING_RANGE.findall(expression)
        intervals = UPDATE_INTERVAL.findall(expression)
        if len(ranges) > 1 or len(intervals) > 1:
            raise ValueError(f"duplicate tracking policy for {name}")
        policies[name] = {
            "client_tracking_range": int(ranges[0]) if ranges else 5,
            "update_interval": parse_interval(intervals[0]) if intervals else 3,
            "track_deltas": name not in NO_DELTA_TYPES,
        }
    return policies


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("vanilla_entity_types", type=Path)
    parser.add_argument("pumpkin_entities", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()

    policies = extract(args.vanilla_entity_types.read_text(encoding="utf-8"))
    entities = json.loads(args.pumpkin_entities.read_text(encoding="utf-8"))
    missing = sorted(set(entities) - set(policies))
    unexpected = sorted(set(policies) - set(entities))
    if missing or unexpected:
        raise SystemExit(
            f"entity registry mismatch: missing={missing}, unexpected={unexpected}"
        )

    args.output.write_text(
        json.dumps(policies, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


if __name__ == "__main__":
    main()
