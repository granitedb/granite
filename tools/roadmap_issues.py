#!/usr/bin/env python3
"""Turn docs/ROADMAP.md into GitHub labels, milestones, issues and a project board.

  roadmap_issues.py plan                  parse and print a summary (no network)
  roadmap_issues.py create OWNER/REPO     create everything with the gh CLI
  roadmap_issues.py update OWNER/REPO     rewrite the body and labels of existing task issues

docs/ROADMAP.md is the index (order, tracks, one-line summary). The full text of
each task comes from its section in docs/tasks/phase-*.md.

`create` is resumable: it records what it made in tools/.roadmap_state.json and
skips anything already there or already on GitHub under the same title.
"""
import json
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ROADMAP = ROOT / "docs" / "ROADMAP.md"
STATE = ROOT / "tools" / ".roadmap_state.json"
TASKS_DIR = ROOT / "docs" / "tasks"

TRACK_NAMES = {
    "A": "storage",
    "B": "consensus and cluster",
    "C": "data model and client",
    "D": "simulation and infrastructure",
}
PHASE_4A = {"E4.1", "E4.2", "E4.3"}


def parse():
    phases, epics, tasks = {}, [], []
    phase = epic = None
    lines = ROADMAP.read_text().splitlines()
    for i, line in enumerate(lines):
        m = re.match(r"^## Phase (\d+)\. (.+?) \((.+)\)\s*$", line)
        if m:
            phase = m.group(1)
            exit_text = ""
            for nxt in lines[i + 1 : i + 4]:
                if nxt.startswith("Exit:"):
                    exit_text = nxt[len("Exit:") :].strip()
            phases[phase] = {"title": m.group(2), "note": m.group(3), "exit": exit_text}
            continue
        m = re.match(r"^### (E\d+\.\d+) (.+)$", line)
        if m:
            epic = {"id": m.group(1), "title": m.group(2), "phase": m.group(1)[1:].split(".")[0]}
            epics.append(epic)
            continue
        m = re.match(r"^\| (T\d+\.\d+\.\d+) \| (.+?) \| (.+?) \| (.+?) \|\s*$", line)
        if m and epic:
            tid, title, track, done = (s.strip() for s in m.groups())
            tasks.append(
                {
                    "id": tid,
                    "title": title,
                    "tracks": [t.strip() for t in track.split(",")],
                    "done": done,
                    "epic": epic["id"],
                    "phase": epic["phase"],
                }
            )
    return phases, epics, tasks


def parse_specs():
    """Return ({task id: (title, body)}, standard gates text) from docs/tasks."""
    specs = {}
    for path in sorted(TASKS_DIR.glob("phase-*.md")):
        tid = title = None
        body = []
        for line in path.read_text().splitlines() + ["## end"]:
            if line.startswith("## "):
                if tid:
                    specs[tid] = (title, "\n".join(body).strip())
                m = re.match(r"^## (T\d+\.\d+\.\d+) (.+)$", line)
                tid, title, body = (m.group(1), m.group(2).strip(), []) if m else (None, None, [])
            elif tid:
                body.append(line)
    readme = (TASKS_DIR / "README.md").read_text()
    standard = readme.split("<!-- standard:begin -->")[1].split("<!-- standard:end -->")[0].strip()
    return specs, standard


def milestone_name(phases, epic_id):
    phase = epic_id[1:].split(".")[0]
    title = phases[phase]["title"]
    if phase == "4":
        return "Phase 4a: Static multi-shard cluster" if epic_id in PHASE_4A else "Phase 4b: Tablet lifecycle"
    return f"Phase {phase}: {title}"


def task_kind(task):
    text = (task["title"] + " " + task["done"]).lower()
    # Whole words only: "report" must not match "reporting" or "reported".
    def has(*words):
        return re.search(r"\b(" + "|".join(words) + r")\b", text) is not None

    if has("model-check", "annotate", "specification"):
        return "kind:spec"
    if has("benchmark", "experiment", "report"):
        return "kind:bench"
    if has("checker", "simulator", "fuzz", "fuzzed", "fuzzer"):
        return "kind:test"
    return "kind:impl"


def task_labels(task):
    labels = [f"phase:{task['phase']}", task_kind(task)]
    # A task owned by "all" belongs to every track.
    tracks = TRACK_NAMES if "all" in task["tracks"] else task["tracks"]
    for t in tracks:
        if t in TRACK_NAMES:
            labels.append(f"track:{t}")
    if re.search(r"\bG[123]\b", task["title"] + " " + task["done"]):
        labels.append("gate")
    return labels


def task_body(task, epic, specs, standard):
    tracks = ", ".join(f"{t} ({TRACK_NAMES[t]})" if t in TRACK_NAMES else t for t in task["tracks"])
    spec = specs[task["id"]][1]
    return (
        f"**Epic:** {epic['id']} {epic['title']}\n"
        f"**Track:** {tracks}\n"
        f"**Summary:** {task['done']}\n\n"
        f"{spec}\n\n"
        f"{standard}\n\n"
        f"## Source\n\n"
        f"This text is generated from `docs/tasks/phase-{task['phase']}.md` ({task['id']}). "
        f"Change it there, not here.\n"
    )


def spec_problems(tasks, specs):
    problems = [f"{t['id']}: no section in docs/tasks" for t in tasks if t["id"] not in specs]
    problems += [f"{i}: section has no task in the roadmap" for i in specs if i not in {t["id"] for t in tasks}]
    for t in tasks:
        if t["id"] not in specs:
            continue
        title, body = specs[t["id"]]
        if title != t["title"]:
            problems.append(f"{t['id']}: title differs from the roadmap")
        for part in ("**Goal.**", "**Work.**", "**Not in this card.**", "**Depends on.**", "**Gates.**"):
            if part not in body:
                problems.append(f"{t['id']}: missing {part}")
        if body.count("- [ ]") < 4:
            problems.append(f"{t['id']}: fewer than 4 gates")
    return problems


def plan():
    phases, epics, tasks = parse()
    print(f"phases: {len(phases)}  epics: {len(epics)}  tasks: {len(tasks)}")
    by_ms = {}
    for e in epics:
        by_ms.setdefault(milestone_name(phases, e["id"]), [0, 0])[0] += 1
    for t in tasks:
        by_ms[milestone_name(phases, t["epic"])][1] += 1
    for name, (ne, nt) in by_ms.items():
        print(f"  {name}: {ne} epics, {nt} tasks")
    ids = [t["id"] for t in tasks]
    dupes = sorted({i for i in ids if ids.count(i) > 1})
    print("duplicate task ids:", dupes or "none")
    empty = [e["id"] for e in epics if not any(t["epic"] == e["id"] for t in tasks)]
    print("epics without tasks:", empty or "none")
    kinds = {}
    for t in tasks:
        kinds[task_kind(t)] = kinds.get(task_kind(t), 0) + 1
    print("kinds:", kinds)
    specs, _ = parse_specs()
    problems = spec_problems(tasks, specs)
    gates = sum(body.count("- [ ]") for _, body in specs.values())
    print(f"task specifications: {len(specs)}  gates: {gates}")
    print("specification problems:", problems or "none")
    return not problems


def gh(*args, input_text=None):
    for attempt in range(5):
        r = subprocess.run(["gh", *args], input=input_text, capture_output=True, text=True)
        if r.returncode == 0:
            return r.stdout.strip()
        if "rate limit" in r.stderr.lower() or "secondary" in r.stderr.lower():
            time.sleep(60 * (attempt + 1))
            continue
        raise SystemExit(f"gh {' '.join(args[:4])} failed: {r.stderr.strip()}")
    raise SystemExit("gave up after repeated rate limiting")


def create(repo):
    phases, epics, tasks = parse()
    specs, standard = parse_specs()
    if spec_problems(tasks, specs):
        raise SystemExit("task specifications are incomplete; run `plan`")
    state = json.loads(STATE.read_text()) if STATE.exists() else {}
    state.setdefault("labels", []), state.setdefault("milestones", {}), state.setdefault("issues", {})

    def save():
        STATE.write_text(json.dumps(state, indent=1))

    labels = {f"phase:{p}": "1d76db" for p in phases}
    labels.update({f"track:{t}": "0e8a16" for t in TRACK_NAMES})
    labels.update({"kind:spec": "5319e7", "kind:impl": "c5def5", "kind:test": "fbca04", "kind:bench": "d93f0b"})
    labels.update({"epic": "3e4b9e", "gate": "b60205"})
    for name, color in labels.items():
        if name not in state["labels"]:
            gh("label", "create", name, "--repo", repo, "--color", color, "--force")
            state["labels"].append(name)
            save()

    for e in epics:
        name = milestone_name(phases, e["id"])
        if name not in state["milestones"]:
            desc = phases[e["phase"]]["exit"]
            out = gh("api", f"repos/{repo}/milestones", "-f", f"title={name}", "-f", f"description={desc}", "--jq", ".number")
            state["milestones"][name] = int(out)
            save()

    epic_by_id = {e["id"]: e for e in epics}

    # Issues already on GitHub, by title, so a lost state file cannot cause duplicates.
    listed = gh("issue", "list", "--repo", repo, "--state", "all", "--limit", "1000", "--json", "number,title")
    existing = {i["title"]: i["number"] for i in json.loads(listed or "[]")}

    def make_issue(key, title, body, label_list, milestone):
        if key in state["issues"]:
            return state["issues"][key]
        if title in existing:
            state["issues"][key] = existing[title]
            save()
            return existing[title]
        args = ["issue", "create", "--repo", repo, "--title", title, "--body-file", "-", "--milestone", milestone]
        for l in label_list:
            args += ["--label", l]
        url = gh(*args, input_text=body)
        number = int(url.rstrip("/").split("/")[-1])
        state["issues"][key] = number
        save()
        time.sleep(1.5)
        return number

    for t in tasks:
        epic = epic_by_id[t["epic"]]
        body = task_body(t, epic, specs, standard)
        make_issue(t["id"], f"{t['id']} {t['title']}", body, task_labels(t), milestone_name(phases, epic["id"]))

    for e in epics:
        children = [t for t in tasks if t["epic"] == e["id"]]
        checklist = "\n".join(f"- [ ] #{state['issues'][t['id']]} {t['id']}" for t in children)
        body = (
            f"Epic {e['id']} of phase {e['phase']} ({phases[e['phase']]['title']}).\n\n"
            f"**Phase exit criteria:** {phases[e['phase']]['exit'] or 'see roadmap'}\n\n"
            f"## Tasks\n\n{checklist}\n\n"
            f"Source: `docs/ROADMAP.md`.\n"
        )
        make_issue(e["id"], f"{e['id']} {e['title']}", body, ["epic", f"phase:{e['phase']}"], milestone_name(phases, e["id"]))

    print(f"done: {len(state['issues'])} issues in {repo}")


def update(repo):
    phases, epics, tasks = parse()
    specs, standard = parse_specs()
    if spec_problems(tasks, specs):
        raise SystemExit("task specifications are incomplete; run `plan`")
    state = json.loads(STATE.read_text())
    epic_by_id = {e["id"]: e for e in epics}
    kinds = ["kind:spec", "kind:impl", "kind:test", "kind:bench"]
    for t in tasks:
        number = str(state["issues"][t["id"]])
        labels = task_labels(t)
        stale = [k for k in kinds if k not in labels]
        gh(
            "issue", "edit", number, "--repo", repo, "--body-file", "-",
            "--add-label", ",".join(labels), "--remove-label", ",".join(stale),
            input_text=task_body(t, epic_by_id[t["epic"]], specs, standard),
        )
        time.sleep(1)
    print(f"done: {len(tasks)} task issues updated in {repo}")


if __name__ == "__main__":
    if len(sys.argv) >= 2 and sys.argv[1] == "plan":
        sys.exit(0 if plan() else 1)
    elif len(sys.argv) == 3 and sys.argv[1] == "update":
        update(sys.argv[2])
    elif len(sys.argv) == 3 and sys.argv[1] == "create":
        create(sys.argv[2])
    else:
        sys.exit(__doc__)
