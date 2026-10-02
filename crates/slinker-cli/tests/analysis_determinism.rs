mod common;

use common::{assert_success, slinker};
use serde_json::Value;
use std::ffi::OsStr;

const JOB_COUNTS: [&str; 4] = ["1", "2", "5", "16"];

fn normalized(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("private:") {
        out.push_str(&rest[..start]);
        out.push_str("private:*");
        let tail = &rest[start + "private:".len()..];
        let label = tail
            .find(|c: char| !(c.is_ascii_hexdigit() || c == '.' || c == ':'))
            .unwrap_or(tail.len());
        rest = &tail[label..];
    }
    out.push_str(rest);
    out
}

fn strings(value: &Value, field: &str) -> String {
    normalized(&value[field].to_string())
}

fn schedule_independent_view(document: &Value) -> Vec<String> {
    let mut components = document["components"]
        .as_array()
        .expect("components")
        .iter()
        .map(|component| {
            let mut members = component["members"]
                .as_array()
                .expect("members")
                .iter()
                .map(|member| normalized(member["id"].as_str().expect("member id")))
                .collect::<Vec<_>>();
            members.sort();
            format!("{}|{}", members.join(","), component["class"])
        })
        .collect::<Vec<_>>();
    components.sort();
    let mut edges = document["edges"]
        .as_array()
        .expect("edges")
        .iter()
        .map(|edge| {
            normalized(&format!(
                "{} -> {} [{}] {}",
                edge["from_member"], edge["to_member"], edge["reason"], edge["detail"]
            ))
        })
        .collect::<Vec<_>>();
    edges.sort();
    let mut diagnostics = document["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|diagnostic| normalized(&diagnostic.to_string()))
        .collect::<Vec<_>>();
    diagnostics.sort();
    let mut view = components;
    view.push("--edges--".into());
    view.extend(edges);
    view.push("--diagnostics--".into());
    view.extend(diagnostics);
    view.push(strings(document, "roots"));
    view.push(strings(document, "packages"));
    view.push(strings(document, "stats"));
    view
}

fn analyze(package: &str, jobs: &str) -> Value {
    let output = slinker(&[
        OsStr::new("analyze"),
        OsStr::new(package),
        OsStr::new("--json"),
        OsStr::new("--jobs"),
        OsStr::new(jobs),
    ]);
    assert_success(&output, &format!("analyze {package} --jobs {jobs}"));
    serde_json::from_slice(&output.stdout).expect("analysis JSON")
}

fn assert_independent_of_jobs(package: &str) {
    let reference = schedule_independent_view(&analyze(package, JOB_COUNTS[0]));
    for jobs in &JOB_COUNTS[1..] {
        let view = schedule_independent_view(&analyze(package, jobs));
        assert_eq!(reference.len(), view.len(), "{package} --jobs {jobs}");
        for (expected, actual) in reference.iter().zip(&view) {
            assert_eq!(expected, actual, "{package} --jobs {jobs}");
        }
    }
}

#[test]
fn compiler_analysis_is_independent_of_the_job_count() {
    assert_independent_of_jobs("compiler");
}

#[test]
fn grid_analysis_is_independent_of_the_job_count() {
    assert_independent_of_jobs("grid");
}
