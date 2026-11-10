"""CSV summary: a no-AI tool. It never calls the broker, so its manifest declares no egress at all."""

import re

from pigeonpost_nest import NestError, serve

NUMBER = re.compile(r"^-?\d+(\.\d+)?$")


def split_line(line, delimiter):
    """Split one CSV line, honouring double-quoted cells ("a, b" and "" escapes)."""
    cells, cell, quoted, i = [], "", False, 0
    while i < len(line):
        c = line[i]
        if quoted:
            if c == '"' and line[i + 1 : i + 2] == '"':
                cell += '"'
                i += 1
            elif c == '"':
                quoted = False
            else:
                cell += c
        elif c == '"':
            quoted = True
        elif c == delimiter:
            cells.append(cell.strip())
            cell = ""
        else:
            cell += c
        i += 1
    cells.append(cell.strip())
    return cells


def summarize(csv, delimiter=",", places=2):
    lines = [l for l in re.split(r"\r?\n", csv) if l.strip()]
    if not lines:
        raise NestError("invalid_operation_payload", "the table is empty")
    columns = split_line(lines[0], delimiter)
    rows = [split_line(l, delimiter) for l in lines[1:]]
    totals = {}
    for c, name in enumerate(columns):
        values = [r[c] for r in rows if c < len(r) and r[c] != ""]
        if values and all(NUMBER.match(v) for v in values):
            total = round(sum(float(v) for v in values), places)
            totals[name] = int(total) if total == int(total) else total
    return {"rows": len(rows), "columns": columns, "totals": totals}


def summarize_job(job):
    csv = job.payload.get("csv")
    if not isinstance(csv, str) or not csv.strip():
        raise NestError("invalid_operation_payload")
    delimiter = job.payload.get("delimiter") or ","
    places = job.config.get("decimal_places", 2)
    places = places if isinstance(places, int) and not isinstance(places, bool) else 2
    return {"answer": summarize(csv, delimiter, places), "sources": [], "model_used": False}


HANDLERS = {"csv.summarize@1.0": summarize_job}

if __name__ == "__main__":
    serve(HANDLERS)
