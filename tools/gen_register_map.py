#!/usr/bin/env python3
"""Generate a modbus-proxy register map from the Sigenergy HA integration.

The register definitions are read by parsing the Python source (ast), the
module is never imported or executed, so Home Assistant is not needed.

Usage: python3 -I tools/gen_register_map.py \
           Sigenergy-Local-Modbus/custom_components/sigen/modbusregisterdefinitions.py \
           registers/sigenergy.json
"""

import ast
import json
import sys

# Home Assistant unit constants used by the definitions
UNITS = {
    "PERCENTAGE": "%",
    "UnitOfElectricCurrent.AMPERE": "A",
    "UnitOfElectricPotential.VOLT": "V",
    "UnitOfEnergy.KILO_WATT_HOUR": "kWh",
    "UnitOfFrequency.HERTZ": "Hz",
    "UnitOfPower.KILO_WATT": "kW",
    "UnitOfTemperature.CELSIUS": "°C",
}

ACCESS = {"READ_ONLY": "ro", "HOLDING": "rw", "WRITE_ONLY": "wo"}

# Corrections to the integration's definitions, checked against
# Modbus_reference_documentation/Sigenergy Modbus Protocol EN_V2.9.pdf
CORRECTIONS = {
    # Power factor is signed (S16) in the protocol; as U16 a factor of
    # -0.951 reads as 64.585.
    31023: {"type": "s16"},
}


def unit(node):
    if isinstance(node, ast.Constant):
        return node.value
    name = ast.unparse(node)
    if name not in UNITS:
        raise SystemExit(f"unknown unit constant {name}")
    return UNITS[name]


def registers(tree):
    for node in tree.body:
        if not (
            isinstance(node, ast.Assign)
            and isinstance(node.value, ast.Dict)
            and node.targets[0].id.endswith("_REGISTERS")
        ):
            continue
        group = node.targets[0].id.removesuffix("_REGISTERS").lower()
        for key, call in zip(node.value.keys, node.value.values):
            kw = {k.arg: k.value for k in call.keywords}
            entry = {
                "address": ast.literal_eval(kw["address"]),
                "count": ast.literal_eval(kw["count"]),
                "type": ast.unparse(kw["data_type"]).split(".")[1].lower(),
                "gain": ast.literal_eval(kw["gain"]),
                "access": ACCESS[ast.unparse(kw["register_type"]).split(".")[1]],
                "name": key.value,
                "group": group,
            }
            if "unit" in kw:
                entry["unit"] = unit(kw["unit"])
            if "description" in kw:
                entry["description"] = ast.literal_eval(kw["description"])
            entry.update(CORRECTIONS.get(entry["address"], {}))
            yield entry


def main(source, target):
    tree = ast.parse(open(source, encoding="utf-8").read())
    regs = sorted(registers(tree), key=lambda r: r["address"])
    used = {}
    for r in regs:
        for a in range(r["address"], r["address"] + r["count"]):
            if a in used:
                raise SystemExit(f"register {a} defined by {used[a]} and {r['name']}")
            used[a] = r["name"]
    out = {
        "source": "https://github.com/TypQxQ/Sigenergy-Local-Modbus "
        "custom_components/sigen/modbusregisterdefinitions.py",
        "license": "MIT, Copyright (c) 2025 Andrei Ignat, see LICENSE.sigenergy",
        "registers": regs,
    }
    with open(target, "w", encoding="utf-8") as f:
        json.dump(out, f, indent=1, ensure_ascii=False)
        f.write("\n")
    print(f"{len(regs)} registers written to {target}")


if __name__ == "__main__":
    main(*sys.argv[1:])
