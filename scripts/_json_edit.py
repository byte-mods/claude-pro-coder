#!/usr/bin/env python3
"""Dependency-free JSON editor for the install pipeline.

Shared by install-mcp.sh (mcpServers.lens) and install-hooks.sh (hooks.PreToolUse).
A twin implementation lives at scripts/_json_edit.js; the two MUST stay
behaviourally identical — _lib.sh picks whichever runtime the machine has, so a
divergence would make the installer's behaviour depend on what is on $PATH.

Usage:
  _json_edit.py <file> get          <dotpath>
  _json_edit.py <file> set          <dotpath> <json-value>   [--dry-run]
  _json_edit.py <file> unset        <dotpath>                [--dry-run]
  _json_edit.py <file> hook-upsert  <event> <matcher> <cmd>  [--dry-run]
  _json_edit.py <file> hook-remove  <event> <marker>         [--dry-run]

`set` prunes nothing; `unset` removes now-empty parent objects so we never leave
an orphan `"mcpServers": {}` behind. `hook-remove` matches any hook whose command
*contains* <marker>, so the caller can key off a stable path fragment.

Exit codes: 0 ok, 1 error, 3 `get` found nothing.
Mutating ops print CHANGED or NOCHANGE on stdout so bash can branch without
re-reading the file.
"""

import json
import os
import sys
import tempfile


def die(msg):
    print("_json_edit: " + msg, file=sys.stderr)
    sys.exit(1)


def load(path):
    """Missing file -> {}. Malformed file -> abort; we never overwrite a config
    we could not parse, because that would silently destroy user state."""
    if not os.path.exists(path):
        return {}
    try:
        with open(path, "r", encoding="utf-8") as f:
            text = f.read()
    except OSError as e:
        die("cannot read %s: %s" % (path, e))
    if text.strip() == "":
        return {}
    try:
        data = json.loads(text)
    except ValueError as e:
        die("%s is not valid JSON: %s. Refusing to overwrite." % (path, e))
    if not isinstance(data, dict):
        die("%s is not a JSON object (got %s); refusing to merge."
            % (path, type(data).__name__))
    return data


def save(path, data, dry_run):
    if dry_run:
        return
    out_dir = os.path.dirname(os.path.abspath(path)) or "."
    try:
        os.makedirs(out_dir, exist_ok=True)
    except OSError as e:
        die("cannot create %s: %s" % (out_dir, e))
    # Atomic: stage a sibling tmp file on the same filesystem, then replace.
    fd, tmp = tempfile.mkstemp(prefix=".json-edit.staging.", dir=out_dir, text=True)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=2, ensure_ascii=False)
            f.write("\n")
        os.replace(tmp, path)
    except Exception:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def get_path(data, parts):
    cur = data
    for p in parts:
        if not isinstance(cur, dict) or p not in cur:
            return None, False
        cur = cur[p]
    return cur, True


def set_path(data, parts, value):
    cur = data
    for p in parts[:-1]:
        nxt = cur.get(p)
        if nxt is None:
            nxt = {}
            cur[p] = nxt
        elif not isinstance(nxt, dict):
            die("cannot descend into '%s' — it is not an object." % p)
        cur = nxt
    cur[parts[-1]] = value


def unset_path(data, parts):
    """Remove the leaf, then walk back up pruning containers we emptied."""
    chain = [data]
    cur = data
    for p in parts[:-1]:
        nxt = cur.get(p)
        if not isinstance(nxt, dict):
            return False
        chain.append(nxt)
        cur = nxt
    if parts[-1] not in cur:
        return False
    del cur[parts[-1]]
    for i in range(len(chain) - 1, 0, -1):
        if chain[i] == {}:
            del chain[i - 1][parts[i - 1]]
        else:
            break
    return True


def hook_entries(data, event):
    hooks = data.get("hooks")
    if hooks is not None and not isinstance(hooks, dict):
        die("existing 'hooks' is not an object; refusing to merge.")
    lst = (hooks or {}).get(event)
    if lst is not None and not isinstance(lst, list):
        die("existing hooks.%s is not an array; refusing to merge." % event)
    return list(lst or [])


def main():
    argv = [a for a in sys.argv[1:] if a != "--dry-run"]
    dry_run = "--dry-run" in sys.argv[1:]
    if len(argv) < 2:
        die("usage: _json_edit.py <file> <op> [args...]")
    path, op, rest = argv[0], argv[1], argv[2:]
    data = load(path)

    if op == "get":
        if len(rest) != 1:
            die("get takes <dotpath>")
        value, found = get_path(data, rest[0].split("."))
        if not found:
            sys.exit(3)
        print(json.dumps(value, ensure_ascii=False))
        return

    if op == "set":
        if len(rest) != 2:
            die("set takes <dotpath> <json-value>")
        try:
            value = json.loads(rest[1])
        except ValueError as e:
            die("value is not valid JSON: %s" % e)
        before, found = get_path(data, rest[0].split("."))
        if found and before == value:
            print("NOCHANGE")
            return
        set_path(data, rest[0].split("."), value)
        save(path, data, dry_run)
        print("CHANGED")
        return

    if op == "unset":
        if len(rest) != 1:
            die("unset takes <dotpath>")
        if not unset_path(data, rest[0].split(".")):
            print("NOCHANGE")
            return
        save(path, data, dry_run)
        print("CHANGED")
        return

    if op == "hook-upsert":
        if len(rest) != 3:
            die("hook-upsert takes <event> <matcher> <command>")
        event, matcher, command = rest
        entries = hook_entries(data, event)
        target = {"matcher": matcher,
                  "hooks": [{"type": "command", "command": command}]}
        for i, e in enumerate(entries):
            if isinstance(e, dict) and e.get("matcher") == matcher:
                if e == target:
                    print("NOCHANGE")
                    return
                entries[i] = target
                break
        else:
            entries.append(target)
        set_path(data, ["hooks", event], entries)
        save(path, data, dry_run)
        print("CHANGED")
        return

    if op == "hook-remove":
        if len(rest) != 2:
            die("hook-remove takes <event> <marker>")
        event, marker = rest
        entries = hook_entries(data, event)
        kept = []
        for e in entries:
            if not isinstance(e, dict):
                kept.append(e)
                continue
            inner = e.get("hooks")
            if not isinstance(inner, list):
                kept.append(e)
                continue
            survivors = [h for h in inner
                         if not (isinstance(h, dict)
                                 and marker in str(h.get("command", "")))]
            if not survivors:
                continue  # drop the whole matcher block; it held only our hook
            if len(survivors) != len(inner):
                e = dict(e)
                e["hooks"] = survivors
            kept.append(e)
        if kept == entries:
            print("NOCHANGE")
            return
        if kept:
            set_path(data, ["hooks", event], kept)
        else:
            unset_path(data, ["hooks", event])
        save(path, data, dry_run)
        print("CHANGED")
        return

    die("unknown op: %s" % op)


if __name__ == "__main__":
    main()
