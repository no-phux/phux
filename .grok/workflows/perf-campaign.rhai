let meta = #{
    name: "perf-campaign",
    description: "Survey wire, reconnect, feel, and mobile perf, then verify a ledger one lap can ship",
    phases: [
        #{ title: "Survey", detail: "one reader per lane" },
        #{ title: "Verify", detail: "one skeptic per finding" },
        #{ title: "Ledger", detail: "ranked work a single lap can ship" },
    ],
};

let findings_schema = #{
    "type": "object", "required": ["findings"],
    "properties": #{
        "findings": #{ "type": "array", "maxItems": 3, "items": #{
            "type": "object", "required": ["lane", "file", "issue", "evidence"],
            "properties": #{
                "lane": #{ "type": "string" },
                "file": #{ "type": "string" },
                "issue": #{ "type": "string" },
                "evidence": #{ "type": "string" },
            },
        }},
    },
};

let verdict_schema = #{
    "type": "object", "required": ["real", "reason", "evidence"],
    "properties": #{
        "real": #{ "type": "boolean" },
        "reason": #{ "type": "string" },
        "evidence": #{ "type": "string" },
    },
};

fn trimmed(s) {
    if type_of(s) == "string" {
        s.trim();
        s
    } else {
        ""
    }
}

fn lane_ok(lane) {
    lane == "wire" || lane == "reconnect" || lane == "feel" || lane == "mobile"
}

let note = "";
if args != () && args.note != () {
    note = args.note;
}

let guard = "Read the code with grep and read_file before you answer. Do not invent timings or frame times. ";
guard += "Do not edit files. Do not touch clients/cockpit. Do not aim a build at the production socket, PHUX_SOCKET, or PHUX_PROFILE=default. ";
guard += "Do not rewrite benchmarks/icount-baseline.json and do not baseline icount from this arm64 host. ";
guard += "Do not put cells on the wire and do not start a protobuf wire break. No new dependencies. ";
guard += "An empty findings list is valid only after you have opened the files. ";
if note != "" {
    guard += "Operator note: " + note + ". ";
}

phase("Survey");
let lanes = ["wire", "reconnect", "feel", "mobile"];
let homes = [
    "crates/phux-protocol, crates/phux-server output path, and crates/phux-client-core. Changes here are on the icount lane.",
    "crates/phux-client-runtime reconnect ladder and connection driver, plus crates/phux/src/commands/attach.rs. phux-client-runtime is not on the icount lane.",
    "clients/desktop and crates/phux-tui. The desk already skips chrome on paint-only wakes. Do not touch clients/cockpit.",
    "/Users/phall/workspace/phux-mobile. Do not bump PHUX_REV unless the pin itself is the finding. Do not hand-edit Derived or build trees.",
];
let jobs = [];
let i = 0;
for lane in lanes {
    let prompt = guard;
    prompt += "Lane: " + lane + ". Home: " + homes[i] + ". ";
    prompt += "Report at most 3 concrete perf findings that are not already fixed on main. ";
    prompt += "Each finding needs lane (exactly this lane name), file, issue, and evidence quoting the code you read. ";
    prompt += "Prefer a reconnect stall, a blank during reconnect, or a hot-path allocation over a rewrite.";
    jobs.push(#{
        prompt: prompt,
        label: "survey:" + lane,
        capability_mode: "read-only",
        output_schema: findings_schema,
    });
    i += 1;
}
let surveyed = parallel(jobs);

let findings = [];
let off_lane = 0;
for r in surveyed {
    if r != () && r.success && type_of(r.output) == "map" && type_of(r.output.findings) == "array" {
        for f in r.output.findings {
            if type_of(f) == "map" && lane_ok(f.lane) && f.file != () && f.issue != () && f.evidence != () {
                findings.push(f);
            } else {
                off_lane += 1;
            }
        }
    }
}
if off_lane > 0 {
    log(off_lane.to_string() + " findings dropped for a bad lane or missing evidence");
}

let kept = [];
let n = 0;
for f in findings {
    if n < 6 {
        kept.push(f);
    }
    n += 1;
}
if findings.len() > 6 {
    log((findings.len() - 6).to_string() + " findings dropped at the cap of 6");
}
if kept.len() == 0 {
    complete(#{ summary: "No verified findings.", confirmed: [] });
}

phase("Verify");
let vjobs = [];
for f in kept {
    let prompt = "Adversarially verify this perf finding by reading the file yourself. ";
    prompt += "Set real=true only when the code you opened still has the problem. ";
    prompt += "Otherwise set real=false. Quote the lines in evidence. Do not invent timings. ";
    prompt += json_encode(f);
    vjobs.push(#{
        prompt: prompt,
        label: "verify:" + f.lane,
        capability_mode: "read-only",
        output_schema: verdict_schema,
    });
}
let verdicts = parallel(vjobs);

phase("Ledger");
let confirmed = [];
let j = 0;
for v in verdicts {
    if v != () && v.success && type_of(v.output) == "map" && v.output.real == true {
        let evidence = trimmed(v.output.evidence);
        if evidence != "" {
            let row = kept[j];
            row.evidence = evidence;
            confirmed.push(row);
        }
    }
    j += 1;
}
log(confirmed.len().to_string() + "/" + kept.len().to_string() + " findings survived verification");

let report = "# Perf ledger\n\n";
report += "One lap is one change, in its own worktree, proved, then a PR. Do not land from this run.\n\n";
if confirmed.len() == 0 {
    report += "Nothing verified.\n";
} else {
    for row in confirmed {
        report += "- " + row.lane + " `" + row.file + "`: " + row.issue + "\n";
    }
}
let path = write_scratch_file("perf-ledger.md", report);
complete(#{
    summary: confirmed.len().to_string() + " verified findings",
    confirmed: confirmed,
    path: path,
});
