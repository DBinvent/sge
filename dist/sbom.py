#!/usr/bin/env python3
# [sbom] A copy of yt-cluster-lite's tools/sbom.py (MIT, the same owner), so sge's build and its
# sbom workflow need no other checkout: keep the two alike (owner, 2026-11-17).
"""The CycloneDX 1.5 SBOM of one binary built from a Rust crate, from `cargo metadata`: every
crate the binary links - normal and build dependencies, not dev ones - with its version, purl
and license, and the crate's own as the SBOM's component. Optionally the npm packages a crate
builds in (--npm-lock), and components given whole (--extra, a JSON list). Each project is
listed with its license: one without stops the run (exit 3) - a question of redistribution,
for a person to answer.

    sbom.py --dir <crate dir> --package <crate> --name <binary> --out <file.cdx.json>
            [--features F] [--no-default-features] [--target T | --target all]
            [--own-license EXPR] [--npm-lock web/package-lock.json] [--extra x.json]
            [--rows rows.json --deb <deb> --deb-version <v>]

--own-license: the license of the path crates (one's own) that name theirs only as a file.
--texts-json: each third-party library's license, copyright and notice files as its package
carries them (LICENSE*, COPYING*, COPYRIGHT*, NOTICE*, UNLICENSE*, and its license file),
added to that JSON file; then `sbom.py --render-texts <json> --out <txt> --title <text>` writes
them as one THIRD-PARTY-LICENSES file, each distinct text once, for a package to ship.
--rows: the same libraries as rows, one a library, appended to that JSON list (made if
missing): {deb, deb_version, binary, binary_version, kind, name, version, license, purl} -
what dbinvent.com's /licenses lists, grouped by deb and binary.

Offline (`cargo metadata --offline`) unless --online: a build has fetched the crates.
"""
import argparse
import datetime
import hashlib
import json
import os
import re
import subprocess
import sys
import uuid

# the files of a package that carry its license, copyright or notice text
TEXT_FILE = re.compile(r"^(LICEN[CS]E|COPYING|COPYRIGHT|NOTICE|UNLICENSE)([-._].*)?$", re.I)
TEXT_MAX = 256 * 1024


def metadata(crate_dir, args):
    """cargo metadata of the workspace, with the crate at crate_dir as the current package
    (its features and --no-default-features apply to it)"""
    cmd = ["cargo", "metadata", "--format-version", "1",
           "--manifest-path", os.path.join(crate_dir, "Cargo.toml")]
    if not args.online:
        cmd.append("--offline")
    if args.target != "all":
        cmd += ["--filter-platform", args.target]
    if args.features:
        cmd += ["--features", args.features]
    if args.no_default_features:
        cmd.append("--no-default-features")
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"sbom: cargo metadata failed in {crate_dir}: {r.stderr.strip()[-600:]}")
    return json.loads(r.stdout)


def walk(meta, package):
    """the root package and every package it links: (root id, ids)"""
    pkgs = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    roots = [i for i in meta["workspace_members"] if pkgs[i]["name"] == package]
    if not roots:
        sys.exit(f"sbom: no package {package} in the workspace")
    seen, stack = set(), [roots[0]]
    while stack:
        i = stack.pop()
        if i in seen:
            continue
        seen.add(i)
        for d in nodes[i]["deps"]:
            if {k.get("kind") for k in d["dep_kinds"]} - {"dev"}:
                stack.append(d["pkg"])
    return roots[0], seen, pkgs, nodes


def purl(p):
    return f"pkg:cargo/{p['name']}@{p['version']}"


def ref(p):
    """[review 2026-11-17] the package's bom-ref: its purl, and for one not from crates.io its
    source too - two of one name and version from two sources are two components"""
    src = p.get("source") or "path"
    return purl(p) if src.startswith("registry+https://github.com/rust-lang/crates.io-index") \
        else f"{purl(p)}#{src}"


def license_of(p, own):
    """the crate's licenses as CycloneDX gives them, [] when it names none"""
    if p.get("license"):
        return [{"expression": p["license"]}]
    if p.get("source") is None and own:
        return [{"expression": own}]
    if p.get("license_file"):
        return [{"license": {"name": f"license file {os.path.basename(p['license_file'])} in its source"}}]
    return []


def license_text(lic):
    """one string for a row: the expression, or the named license"""
    return "; ".join(x.get("expression") or x.get("license", {}).get("id") or x.get("license", {}).get("name", "")
                     for x in lic)


def text_files(dirname, extra=None):
    """{file name: text} of the license, copyright and notice files at the top of a package;
    None when the package is not at hand (no such directory)"""
    out = {}
    try:
        names = sorted(os.listdir(dirname))
    except OSError:
        return None
    # [review 2026-11-17] a license file named by an absolute path is taken by its name only
    if extra and os.path.isabs(extra):
        extra = os.path.relpath(extra, dirname) if extra.startswith(os.path.abspath(dirname) + os.sep) else None
    if extra and os.path.isfile(os.path.join(dirname, extra)) and extra not in names:
        names.append(extra)
    for n in names:
        path = os.path.join(dirname, n)
        if (TEXT_FILE.match(os.path.basename(n)) or n == extra) and os.path.isfile(path):
            with open(path, "rb") as f:
                raw = f.read(TEXT_MAX + 1)
            if b"\0" in raw:  # [review 2026-11-17] a binary file of that name is no text
                continue
            text = raw[:TEXT_MAX].decode("utf-8", errors="replace").rstrip() + "\n"
            if len(raw) > TEXT_MAX:  # [review 2026-11-17] said, never cut quietly
                text += "[... cut at 256 KiB here: the whole text is in the package's source]\n"
            out[n] = text
    return out


def add_texts(texts_json, entries):
    """entries [(key, name, version, license, files)] into the JSON file, keyed: a library once"""
    data = {}
    if os.path.exists(texts_json):
        with open(texts_json) as f:
            data = json.load(f)
    for key, name, version, lic, files in entries:
        data.setdefault(key, {"name": name, "version": version, "license": lic, "files": files})
    with open(texts_json + ".tmp", "w") as f:
        json.dump(data, f)
    os.replace(texts_json + ".tmp", texts_json)


def render_texts(texts_json, out, title):
    """the THIRD-PARTY-LICENSES file: a library a block, its files' texts, each distinct text
    written once and named again where another library carries the same"""
    with open(texts_json) as f:
        data = json.load(f)
    entries = sorted(data.values(), key=lambda e: (e["name"].lower(), e["version"]))
    seen = {}
    lines = [title, "", "%d libraries, each with the license, copyright and notice files its package carries "
             "(LICENSE, COPYING, COPYRIGHT, NOTICE ...), written %s." % (len(entries), datetime.date.today().isoformat()),
             "A text two libraries carry alike is written once, at the first, and named at the others.", ""]
    bare = 0
    for e in entries:
        lines.append("=" * 79)
        lines.append("%s %s - %s" % (e["name"], e["version"], e["license"]))
        lines.append("=" * 79)
        if e["files"] is None:  # [review 2026-11-17] not "none in its package" when it was not read
            bare += 1
            lines += ["Its package was not at hand when this was written; its license, as it declares it: %s." % e["license"], ""]
            continue
        if not e["files"]:
            bare += 1
            lines += ["Its package carries no license file; its license, as it declares it: %s." % e["license"], ""]
            continue
        for fname, text in sorted(e["files"].items()):
            h = hashlib.sha256(text.encode()).hexdigest()
            lines.append("--- %s" % fname)
            if h in seen:
                lines += ["(the same text as %s's %s, above)" % seen[h], ""]
            else:
                seen[h] = ("%s %s" % (e["name"], e["version"]), fname)
                lines += [text.rstrip(), ""]
    with open(out + ".tmp", "w") as f:
        f.write("\n".join(lines) + "\n")
    os.replace(out + ".tmp", out)
    print(f"sbom: {len(entries)} libraries' license texts ({len(seen)} distinct, {bare} with none in their package) -> {out}")


def npm_components(lock):
    """the non-dev packages of an npm lock file (lockfileVersion 2 or 3)"""
    with open(lock) as f:
        packages = json.load(f).get("packages", {})
    out = {}
    for path, p in packages.items():
        if not path or p.get("dev") or "node_modules/" not in path:
            continue
        name = path.rsplit("node_modules/", 1)[1]
        # [review 2026-11-17] a scope's "@" percent-encoded, as the purl spec has it
        url = f"pkg:npm/{name.replace('@', '%40', 1) if name.startswith('@') else name}@{p.get('version', '')}"
        lic = p.get("license")
        out[url] = {"type": "library", "bom-ref": url, "name": name, "version": p.get("version", ""),
                    "purl": url, "licenses": [{"expression": lic}] if isinstance(lic, str) and lic else [],
                    "properties": [{"name": "yt:built-into", "value": "web UI"}]}
    return list(out.values())


def main():
    if "--render-texts" in sys.argv:
        r = argparse.ArgumentParser(description="the THIRD-PARTY-LICENSES file from --texts-json")
        r.add_argument("--render-texts", required=True)
        r.add_argument("--out", required=True)
        r.add_argument("--title", required=True)
        ra = r.parse_args()
        render_texts(ra.render_texts, ra.out, ra.title)
        return
    a = argparse.ArgumentParser(description="CycloneDX 1.5 SBOM of a Rust binary, from cargo metadata")
    a.add_argument("--dir", required=True)
    a.add_argument("--package", required=True)
    a.add_argument("--name", required=True, help="the binary's name in the SBOM")
    a.add_argument("--out", required=True)
    a.add_argument("--texts-json", default="")
    a.add_argument("--features", default="")
    a.add_argument("--no-default-features", action="store_true")
    a.add_argument("--target", default="x86_64-unknown-linux-gnu", help='a target triple, or "all"')
    a.add_argument("--own-license", default="")
    a.add_argument("--npm-lock", default="")
    a.add_argument("--extra", action="append", default=[])
    a.add_argument("--rows", default="")
    a.add_argument("--deb", default="")
    a.add_argument("--deb-version", default="")
    a.add_argument("--online", action="store_true")
    args = a.parse_args()

    meta = metadata(args.dir, args)
    root_id, ids, pkgs, nodes = walk(meta, args.package)
    root = pkgs[root_id]
    comps, deps = [], []
    for i in sorted(ids - {root_id}, key=lambda i: (pkgs[i]["name"], pkgs[i]["version"])):
        p = pkgs[i]
        c = {"type": "library", "bom-ref": ref(p), "name": p["name"], "version": p["version"],
             "purl": purl(p), "licenses": license_of(p, args.own_license)}
        if p.get("description"):
            c["description"] = " ".join(p["description"].split())
        if p.get("source") is None:
            c["properties"] = [{"name": "yt:source", "value": "path"}]
        elif str(p.get("source", "")).startswith("git+"):
            c["properties"] = [{"name": "yt:source", "value": p["source"]}]
        comps.append(c)
    # the graph: each package and what it links, by bom-ref
    for i in sorted(ids):
        on = sorted({ref(pkgs[d["pkg"]]) if d["pkg"] != root_id else "root" for d in nodes[i]["deps"]
                     if d["pkg"] in ids and {k.get("kind") for k in d["dep_kinds"]} - {"dev"}})
        deps.append({"ref": ref(pkgs[i]) if i != root_id else "root", "dependsOn": on})
    if args.npm_lock:
        npm = npm_components(args.npm_lock)
        comps += npm
        deps[[d["ref"] for d in deps].index("root")]["dependsOn"] += [c["bom-ref"] for c in npm]
    for x in args.extra:
        with open(x) as f:
            extra = json.load(f)
        comps += extra
        deps[[d["ref"] for d in deps].index("root")]["dependsOn"] += [c["bom-ref"] for c in extra]
    top = {"type": "application", "bom-ref": "root", "name": args.name, "version": root["version"],
           "licenses": license_of(root, args.own_license), "purl": purl(root)}
    bom = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": f"urn:uuid:{uuid.uuid4()}",
        "version": 1,
        "metadata": {
            "timestamp": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "tools": {"components": [{"type": "application", "name": "yt-cluster-lite tools/sbom.py"}]},
            "component": top,
            "properties": [{"name": "yt:target", "value": args.target},
                           {"name": "yt:features", "value": ("no default; " if args.no_default_features else "")
                            + (args.features or "default")}],
        },
        "components": comps,
        "dependencies": deps,
    }
    bare = [f"{c['name']} {c.get('version', '')}" for c in [top] + comps if not c.get("licenses")]
    if bare:
        print(f"sbom: {args.name}: no license given for " + ", ".join(bare), file=sys.stderr)
        sys.exit(3)
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    with open(args.out + ".tmp", "w") as f:
        json.dump(bom, f, indent=1)
        f.write("\n")
    os.replace(args.out + ".tmp", args.out)
    if args.texts_json:
        # third-party libraries only: the path crates are one's own, under the package's LICENSE
        # [review 2026-11-17] keyed by the bom-ref: one name and version from two sources are two
        entries = [(ref(pkgs[i]), pkgs[i]["name"], pkgs[i]["version"],
                    license_text(license_of(pkgs[i], "")) or "-",
                    text_files(os.path.dirname(pkgs[i]["manifest_path"]), pkgs[i].get("license_file")))
                   for i in sorted(ids - {root_id}) if pkgs[i].get("source") is not None]
        if args.npm_lock:
            modules = os.path.join(os.path.dirname(os.path.abspath(args.npm_lock)), "node_modules")
            entries += [(f"npm:{c['name']}@{c['version']}", c["name"], c["version"], license_text(c["licenses"]) or "-",
                         text_files(os.path.join(modules, c["name"]))) for c in npm_components(args.npm_lock)]
        add_texts(args.texts_json, entries)
    if args.rows:
        rows = json.load(open(args.rows)) if os.path.exists(args.rows) else []
        for c in comps:
            kind = c["purl"].split(":", 1)[1].split("/", 1)[0] if c.get("purl", "").startswith("pkg:") else "other"
            rows.append({"deb": args.deb, "deb_version": args.deb_version, "binary": args.name,
                         "binary_version": root["version"], "kind": kind, "name": c["name"],
                         "version": c.get("version", ""), "license": license_text(c["licenses"]),
                         "purl": c.get("purl", "")})
        with open(args.rows + ".tmp", "w") as f:
            json.dump(rows, f)
        os.replace(args.rows + ".tmp", args.rows)
    print(f"sbom: {args.name} {root['version']}: {len(comps)} components, each with its license -> {args.out}")


if __name__ == "__main__":
    main()
