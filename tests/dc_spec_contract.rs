//! The Data Center route table against the server's own description [M6.3, D27].
//!
//! `src/api/datacenter/routes.rs` lists every REST route jired calls, named by
//! the `method id` the server gives it. This reads the WADLs recorded under
//! `specs/datacenter/10.7.4/` and checks the table against them, so a route
//! that does not exist, a method spelled wrong, or a query parameter the server
//! never declared is a failing build rather than a failing push.
//!
//! What this cannot check, and why that is all right: the WADL Jersey serves by
//! default is the *simplified* one — "user and core resources only" — which
//! leaves out the generated `OPTIONS` and `HEAD` methods and the
//! `application.wadl` resource itself. Every route jired uses is in it. Before
//! concluding a server lacks one, fetch `…/application.wadl?detail=true`.
//!
//! Nor does it check payloads. This WADL carries an empty `<grammars/>` and not
//! one `element=` attribute, so it describes paths and parameters and no shape
//! at all. What goes in a request body is checked by the end-to-end tests
//! against the recorded fixtures, not here.

use jired::api::datacenter::routes::{self, Module, Route};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// The version of Jira the vendored description came from. Named once: it is
/// in three file names and in the message a failure prints.
const RECORDED: &str = "10.7.4";

/// Parameters jired sends that the server honours without declaring them.
///
/// `expand` is a Jira-wide convention rather than a per-method parameter: the
/// framework reads it on any resource that returns an expandable document, and
/// Jersey — which builds the WADL from method signatures — never sees it. The
/// one jired depends on is `expand=transitions.fields`, which is what says
/// which fields a transition demands; without it a transition that requires a
/// resolution looks like one that does not.
///
/// Each entry is checked to be genuinely undeclared, so an allowlist cannot
/// outlive the reason it was written.
const UNDECLARED_BUT_HONOURED: &[(&str, &str)] = &[("getTransitions", "expand")];

/// One method, as the description declares it.
struct Declared {
    id: String,
    query: BTreeSet<String>,
}

/// Every method in one WADL, keyed by method and module-relative path.
///
/// Keyed on the pair rather than on the id because ids repeat: `getIssue` is a
/// method of both the platform and the Agile API, and `getPropertiesKeys` is a
/// method of half a dozen resources within one.
fn declared(module: Module) -> BTreeMap<(String, String), Declared> {
    let name = match module {
        Module::Api => "api-2",
        Module::Agile => "agile-1.0",
    };
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("specs/datacenter")
        .join(RECORDED)
        .join(format!(
            "jira.corp.example-datacenter-{RECORDED}-{name}.wadl"
        ));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let doc = roxmltree::Document::parse(&text)
        .unwrap_or_else(|e| panic!("{} is not XML: {e}", path.display()));

    let resources = doc
        .descendants()
        .find(|n| n.has_tag_name("resources"))
        .unwrap_or_else(|| panic!("{}: no <resources>", path.display()));

    let mut out = BTreeMap::new();
    walk(resources, "", &mut out);
    assert!(
        !out.is_empty(),
        "{} declared no methods; did the format change?",
        path.display()
    );
    out
}

/// Join nested `<resource path>` segments the way a request does.
fn walk(node: roxmltree::Node, prefix: &str, out: &mut BTreeMap<(String, String), Declared>) {
    for resource in node.children().filter(|c| c.has_tag_name("resource")) {
        // Some resources spell their path with a leading slash and some do not;
        // both mean the same segment.
        let segment = resource.attribute("path").unwrap_or("").trim_matches('/');
        let path = match prefix.is_empty() {
            true => segment.to_string(),
            false => format!("{prefix}/{segment}"),
        };

        for method in resource.children().filter(|c| c.has_tag_name("method")) {
            let query = method
                .children()
                .filter(|c| c.has_tag_name("request"))
                .flat_map(|r| r.children())
                .filter(|p| p.has_tag_name("param") && p.attribute("style") == Some("query"))
                .filter_map(|p| p.attribute("name").map(str::to_string))
                .collect();
            let Some(verb) = method.attribute("name") else {
                continue;
            };
            out.insert(
                (verb.to_string(), path.clone()),
                Declared {
                    id: method.attribute("id").unwrap_or_default().to_string(),
                    query,
                },
            );
        }
        walk(resource, &path, out);
    }
}

fn lookup<'a>(
    spec: &'a BTreeMap<(String, String), Declared>,
    route: &Route,
) -> Option<&'a Declared> {
    spec.get(&(route.method.to_string(), route.path.to_string()))
}

#[test]
fn every_route_jired_uses_is_declared_by_the_server_with_the_same_method() {
    let api = declared(Module::Api);
    let agile = declared(Module::Agile);

    for route in routes::ALL {
        let spec = match route.module {
            Module::Api => &api,
            Module::Agile => &agile,
        };
        if lookup(spec, route).is_some() {
            continue;
        }
        // A path that exists under another method is the likely mistake, and
        // naming it saves the reader a trip to a 184 KiB document.
        let others: Vec<&str> = spec
            .keys()
            .filter(|(_, p)| p == route.path)
            .map(|(m, _)| m.as_str())
            .collect();
        let near = match others.is_empty() {
            true => "and no method at that path at all".to_string(),
            false => format!("but the server declares {others:?} there"),
        };
        panic!(
            "{}: Jira {RECORDED} does not declare {} {}, {near}",
            route.id, route.method, route.path
        );
    }
}

#[test]
fn no_route_sends_a_query_parameter_the_server_does_not_declare() {
    let api = declared(Module::Api);
    let agile = declared(Module::Agile);

    for route in routes::ALL {
        let spec = match route.module {
            Module::Api => &api,
            Module::Agile => &agile,
        };
        let found = lookup(spec, route).expect("checked by the test above");

        for name in route.query {
            if found.query.contains(*name) {
                continue;
            }
            assert!(
                UNDECLARED_BUT_HONOURED.contains(&(route.id, name)),
                "{}: jired sends `{name}` on {} {}, which Jira {RECORDED} does \
                 not declare (it declares {:?}). If the server honours it \
                 anyway, add it to UNDECLARED_BUT_HONOURED with the reason.",
                route.id,
                route.method,
                route.path,
                found.query,
            );
        }
    }
}

/// An allowlist entry that names a parameter the server has since declared is
/// an exception nobody needs any more, and one nobody would think to remove.
#[test]
fn every_allowed_exception_is_still_an_exception() {
    let api = declared(Module::Api);
    let agile = declared(Module::Agile);

    for (id, param) in UNDECLARED_BUT_HONOURED {
        let route = routes::ALL
            .iter()
            .find(|r| r.id == *id)
            .unwrap_or_else(|| panic!("{id} is allowlisted but is not a route"));
        let spec = match route.module {
            Module::Api => &api,
            Module::Agile => &agile,
        };
        let found = lookup(spec, route).expect("checked by the first test");
        assert!(
            !found.query.contains(*param),
            "{id}: Jira {RECORDED} now declares `{param}`; drop it from \
             UNDECLARED_BUT_HONOURED and let the ordinary check cover it"
        );
        assert!(
            route.query.contains(param),
            "{id}: `{param}` is allowlisted but the route does not list it"
        );
    }
}

#[test]
fn the_route_ids_match_the_servers_own_names() {
    let api = declared(Module::Api);
    let agile = declared(Module::Agile);

    for route in routes::ALL {
        let spec = match route.module {
            Module::Api => &api,
            Module::Agile => &agile,
        };
        let found = lookup(spec, route).expect("checked by the first test");
        assert_eq!(
            found.id, route.id,
            "{} {}: the server calls this `{}`, the table calls it `{}`",
            route.method, route.path, found.id, route.id
        );
    }
}

/// The three things jired reaches that the REST API does not offer are listed
/// in `routes::OUTSIDE_THE_SPEC` with a reason each. This checks the list is
/// honest: none of them is a route, and none has quietly become one.
#[test]
fn what_jired_reaches_outside_the_rest_api_is_listed_and_absent_from_it() {
    let api = declared(Module::Api);
    let agile = declared(Module::Agile);

    assert_eq!(
        routes::OUTSIDE_THE_SPEC.len(),
        3,
        "the wizard, the attachment bytes, and the description itself"
    );

    for (path, why) in routes::OUTSIDE_THE_SPEC {
        assert!(!why.is_empty(), "{path} is listed without a reason");
        assert!(
            !routes::ALL.iter().any(|r| r.path == *path),
            "{path} is both a route and an exception to the routes"
        );
        // The paths above are absolute and the description's are
        // module-relative, so a match is a suffix rather than an equality.
        let trimmed = path.trim_start_matches('/');
        for spec in [&api, &agile] {
            for (method, declared_path) in spec.keys() {
                assert!(
                    !trimmed.ends_with(declared_path.as_str()) || declared_path.is_empty(),
                    "{path} is now declared as {method} {declared_path}: \
                     the REST API has grown a route for it, so jired should \
                     use it instead of the web action"
                );
            }
        }
    }
}

/// The fixtures replay the vendored description rather than a summary of it, so
/// what `fetch-spec` writes in a test is what a real server would have sent.
#[test]
fn the_fixture_replays_the_vendored_spec() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for (module, fixture) in [
        ("api-2", "spec-api-2.wadl"),
        ("agile-1.0", "spec-agile-1.0.wadl"),
    ] {
        let vendored = repo.join("specs/datacenter").join(RECORDED).join(format!(
            "jira.corp.example-datacenter-{RECORDED}-{module}.wadl"
        ));
        let replayed = repo.join("tests/fixtures/datacenter").join(fixture);
        assert_eq!(
            std::fs::read(&vendored).unwrap(),
            std::fs::read(&replayed).unwrap(),
            "{} and {} have drifted apart",
            vendored.display(),
            replayed.display()
        );
    }

    // And the third is deliberately missing: a module the server does not serve
    // is a case `fetch-spec` has to report, and a fixture can only pose it by
    // not being there.
    assert!(
        !repo
            .join("tests/fixtures/datacenter/spec-auth-1.wadl")
            .exists(),
        "the absent auth module is what exercises the `skipped` path"
    );
}
