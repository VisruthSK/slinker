use super::*;
use crate::package::{BindingName, PackageName};
use crate::syntax::{ConstructionExprKind, ConstructionTarget, DeclaredCallable, DeclaredDomain};
use oak_semantic::ImportsResolver;

fn parse_source(text: &str) -> ParsedRFile {
    OakParser.parse_binding(SourceId(0), text).unwrap()
}

fn reference_names(parsed: &ParsedRFile) -> Vec<&str> {
    parsed.expressions[0]
        .references
        .iter()
        .map(|reference| reference.name.as_str())
        .collect()
}

#[test]
fn quote_suppresses_ordinary_symbol_uses() {
    let parsed = parse_source("f <- function() quote(foo)");
    assert!(!reference_names(&parsed).contains(&"foo"));
}

#[test]
fn bquote_analyzes_holes_only() {
    let parsed = parse_source("f <- function() bquote(foo + .(bar))");
    let names = reference_names(&parsed);
    assert!(!names.contains(&"foo"));
    assert!(names.contains(&"bar"));
}

#[test]
fn evaluated_quotation_is_live_code_in_the_calling_frame() {
    let parsed = parse_source("f <- function(root) eval(bquote(function(...) path(.(root), ...)))");
    let names = reference_names(&parsed);
    assert!(names.contains(&"path"));
    assert!(!names.contains(&"root"));
    assert!(
        reference_names(&parse_source("f <- function() eval(quote(helper()))")).contains(&"helper")
    );
    assert!(reference_names(&parse_source("f <- function() evalq(helper())")).contains(&"helper"));
}

#[test]
fn callee_shadowed_by_a_non_closure_local_can_reach_the_enclosing_function() {
    let kind = |source| {
        parse_source(source).expressions[0]
            .references
            .iter()
            .find(|reference| reference.name == "path")
            .map(|reference| reference.kind)
    };
    assert_eq!(
        kind("f <- function(path) path(path, 'x')"),
        Some(NameRefKind::MaybeLocal)
    );
    assert_eq!(
        kind("f <- function() { path <- function() 1; path() }"),
        None
    );
}

#[test]
fn dots_elements_are_never_free_names() {
    let parsed = parse_source("f <- function(...) if (!missing(..1)) ..12 else ..x");
    let names = reference_names(&parsed);
    assert!(!names.contains(&"..1"));
    assert!(!names.contains(&"..12"));
    assert!(names.contains(&"..x"));
}

#[test]
fn replacement_call_references_the_replacement_function() {
    let parsed = parse_source("f <- function(x) { substr2(x, 1, 2) <- 'a'; x }");
    let names = reference_names(&parsed);
    assert!(names.contains(&"substr2<-"));
    assert!(!names.contains(&"substr2"));
}

#[test]
fn assigned_value_start_is_the_outer_assignment_value() {
    let source = "`.onLoad` <- function(libname, pkgname) { x <- 1 }";
    let start = assigned_value_start(source).unwrap();
    assert!(source[start..].starts_with("function(libname"));
    assert_eq!(assigned_value_start("f(1)"), None);
}

#[test]
fn slinker_declaration_is_an_inert_lexical_contract() {
    let parsed = parse_source(
        "f <- function(x) { print(x); declare(slinker(x = one_of(s3('foo'), s3('bar', 'parent')))) }",
    );
    let names = reference_names(&parsed);
    for inert in ["slinker", "one_of", "s3"] {
        assert!(!names.contains(&inert), "{inert}");
    }
    assert!(
        parsed.expressions[0]
            .calls
            .iter()
            .all(|call| !["slinker", "s3", "one_of"].contains(&call.callee.as_str()))
    );
    let [declaration] = parsed.declarations.as_slice() else {
        panic!("{:?}", parsed.declarations);
    };
    assert_eq!(declaration.binding.name, "x");
    assert_eq!(
        declaration.domain,
        DeclaredDomain::Classes(vec![
            vec!["foo".into()],
            vec!["bar".into(), "parent".into()]
        ])
    );
    let print = parsed.expressions[0]
        .calls
        .iter()
        .find(|call| call.callee == "print")
        .unwrap();
    assert_eq!(
        parsed.class_domain_for(print.arg_bindings[0].as_ref().unwrap(), print.scope),
        Some(vec![
            vec!["foo".into()],
            vec!["bar".into(), "parent".into()]
        ])
    );
}

#[test]
fn string_declarations_narrow_and_never_mix_with_classes() {
    let parsed = parse_source(
        "f <- function(name) { declare(slinker(name = strings('alpha', 'beta'))); g <- function() { declare(slinker(name = strings('beta', 'gamma'))); print(name) } }",
    );
    assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
    let print = parsed.expressions[0]
        .calls
        .iter()
        .find(|call| call.callee == "print")
        .unwrap();
    let binding = print.arg_bindings[0].as_ref().unwrap();
    assert_eq!(
        parsed.string_domain_for(binding, print.scope),
        Some(["beta".to_owned()].into_iter().collect())
    );
    assert_eq!(parsed.class_domain_for(binding, print.scope), None);
    assert!(names_inert(&parsed, "strings"));

    let mixed = parse_source(
        "f <- function(x) { declare(slinker(x = strings('a'))); declare(slinker(x = s3('foo'))) }",
    );
    assert_eq!(mixed.declarations.len(), 1);
    assert!(
        mixed
            .issues
            .iter()
            .any(|issue| issue.kind == SemanticIssueKind::InvalidDeclaration)
    );
    for malformed in [
        "f <- function(x) declare(slinker(x = strings()))",
        "f <- function(x) declare(slinker(x = strings(y)))",
        "f <- function(x) declare(slinker(x = one_of(strings('a'))))",
    ] {
        let parsed = parse_source(malformed);
        assert!(parsed.declarations.is_empty(), "{malformed}");
        assert!(
            parsed
                .issues
                .iter()
                .any(|issue| issue.kind == SemanticIssueKind::InvalidDeclaration),
            "{malformed}"
        );
    }
}

#[test]
fn callable_declarations_name_exact_functions() {
    let parsed = parse_source(
        "f <- function(fun) { declare(slinker(fun = callables(pkg::g, pkg:::h, local_fn))); print(fun) }",
    );
    assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
    let print = parsed.expressions[0]
        .calls
        .iter()
        .find(|call| call.callee == "print")
        .unwrap();
    let callable = |package: Option<&str>, name: &str| DeclaredCallable {
        package: package.map(str::to_owned),
        name: name.to_owned(),
    };
    assert_eq!(
        parsed.callable_domain_for(print.arg_bindings[0].as_ref().unwrap(), print.scope),
        Some(
            [
                callable(Some("pkg"), "g"),
                callable(Some("pkg"), "h"),
                callable(None, "local_fn"),
            ]
            .into_iter()
            .collect()
        )
    );
    assert!(names_inert(&parsed, "callables"));
    for malformed in [
        "f <- function(x) declare(slinker(x = callables()))",
        "f <- function(x) declare(slinker(x = callables('g')))",
        "f <- function(x) declare(slinker(x = callables(g(1))))",
    ] {
        let parsed = parse_source(malformed);
        assert!(parsed.declarations.is_empty(), "{malformed}");
    }
}

fn names_inert(parsed: &ParsedRFile, name: &str) -> bool {
    !reference_names(parsed).contains(&name)
        && parsed.expressions[0]
            .calls
            .iter()
            .all(|call| call.callee != name)
}

#[test]
fn nested_declaration_narrows_the_captured_binding() {
    let parsed = parse_source(
        "f <- function(x) { declare(slinker(x = one_of(s3('foo'), s3('bar')))); g <- function() { declare(slinker(x = s3('foo'))); print(x) } }",
    );
    let print = parsed.expressions[0]
        .calls
        .iter()
        .find(|call| call.callee == "print")
        .unwrap();
    assert_eq!(
        parsed.class_domain_for(print.arg_bindings[0].as_ref().unwrap(), print.scope),
        Some(vec![vec!["foo".into()]])
    );
}

#[test]
fn shadowed_or_malformed_declarations_are_not_contracts() {
    let shadowed = parse_source(
        "f <- function(x) { declare <- function(...) NULL; declare(slinker(x = s3('foo'))) }",
    );
    assert!(shadowed.declarations.is_empty());
    let malformed = parse_source("f <- function(x) declare(slinker(x = s3(klass)))");
    assert!(malformed.declarations.is_empty());
    assert!(
        malformed
            .issues
            .iter()
            .any(|issue| issue.kind == SemanticIssueKind::InvalidDeclaration)
    );
}

#[test]
fn custom_infix_operator_is_a_name_reference() {
    let parsed = parse_source("f <- function(a, b) a %R% (b %in% a)");
    let names = reference_names(&parsed);
    assert!(names.contains(&"%R%"));
    assert!(names.contains(&"%in%"));
}

#[test]
fn data_masked_names_may_be_columns() {
    let parsed = parse_source("f <- function(d) with(d, middle + helper(x))");
    let kinds = parsed.expressions[0]
        .references
        .iter()
        .map(|reference| (reference.name.as_str(), reference.kind))
        .collect::<Vec<_>>();
    assert!(kinds.contains(&("middle", NameRefKind::MaybeLocal)));
    assert!(kinds.contains(&("helper", NameRefKind::MaybeLocal)));
}

#[test]
fn dispatch_frame_variables_are_never_free_names() {
    let parsed = parse_source("Ops.poly <- function(e1, e2) switch(.Generic, `+` = .Class)");
    let names = reference_names(&parsed);
    assert!(!names.contains(&".Generic"));
    assert!(!names.contains(&".Class"));
}

#[test]
fn quotation_evaluated_elsewhere_stays_inert() {
    let parsed = parse_source("f <- function(env) eval(quote(helper()), env)");
    assert!(!reference_names(&parsed).contains(&"helper"));
}

#[test]
fn shadowed_bquote_does_not_receive_base_effects() {
    let context = OakParseContext::new(BTreeSet::from(["bquote".into()]));
    let parsed = OakParser
        .parse_binding_with_context(
            SourceId(0),
            "f <- function() bquote(foo + .(bar))",
            &context,
        )
        .unwrap();
    assert!(reference_names(&parsed).contains(&"foo"));
}

#[test]
fn qualified_base_bquote_receives_base_effects() {
    let parsed = parse_source("f <- function() base::bquote(foo + .(bar))");
    let names = reference_names(&parsed);
    assert!(!names.contains(&"foo"));
    assert!(names.contains(&"bar"));
}

#[test]
fn conditional_local_read_keeps_external_fallthrough_distinct() {
    let parsed = parse_source("f <- function(flag) { if (flag) x <- 1; x }");
    let reference = parsed.expressions[0]
        .references
        .iter()
        .find(|reference| reference.name == "x")
        .unwrap();
    assert_eq!(reference.kind, NameRefKind::ConditionalFallthrough);
}

#[test]
fn local_parameter_never_becomes_external_reference() {
    let parsed = parse_source("f <- function(x) x");
    assert!(!reference_names(&parsed).contains(&"x"));
}

#[test]
fn superassigned_value_is_a_dependency_only_when_free() {
    let values = |text| {
        parse_source(text).expressions[0]
            .effects
            .iter()
            .map(|effect| (effect.kind, effect.value_symbol.clone()))
            .collect::<Vec<_>>()
    };

    let local = values("set <- function(x) value <<- x");
    let free = values("set <- function() value <<- other");

    assert!(!local.is_empty() && !free.is_empty());
    assert!(
        local
            .iter()
            .all(|effect| *effect == (SyntaxEffectKind::SuperAssignment, None))
    );
    assert!(free.iter().all(|effect| {
        *effect == (SyntaxEffectKind::SuperAssignment, Some("other".to_owned()))
    }));
}

#[test]
fn quoted_namespace_access_is_inert() {
    let parsed = parse_source("f <- function() quote(foo::bar)");
    assert!(parsed.expressions[0].package_refs.is_empty());
}

#[test]
fn evaluated_namespace_access_is_recorded() {
    let parsed = parse_source("f <- function() foo::bar()");
    assert!(
        parsed.expressions[0]
            .package_refs
            .iter()
            .any(|reference| reference.package == "foo" && reference.symbol == "bar")
    );
}

#[test]
fn quoted_system_file_is_inert() {
    let parsed = parse_source("f <- function() quote(system.file('data', package = 'foo'))");
    assert!(parsed.expressions[0].resource_refs.is_empty());
}

#[test]
fn imported_bquote_does_not_receive_base_effects() {
    let mut context = OakParseContext::default();
    context.add_import_from("bquote", "fake", "bquote");
    let parsed = OakParser
        .parse_binding_with_context(
            SourceId(0),
            "f <- function() bquote(foo + .(bar))",
            &context,
        )
        .unwrap();
    assert!(reference_names(&parsed).contains(&"foo"));
}

#[test]
fn substitute_quotes_expression_but_evaluates_environment_argument() {
    let parsed = parse_source(
        "f <- function() substitute(quoted_symbol + other_quoted, external_environment)",
    );
    let names = reference_names(&parsed);
    assert!(!names.contains(&"quoted_symbol"));
    assert!(!names.contains(&"other_quoted"));
    assert!(names.contains(&"external_environment"));
}

#[test]
fn attached_search_path_does_not_claim_bare_effect_identity() {
    let context = OakParseContext::default();
    let mut resolver = SlinkerImportsResolver { context: &context };
    assert!(
        resolver
            .resolve_effects("quote", &["some_attached_package".to_owned()])
            .is_none()
    );
}

#[test]
fn definite_local_assignment_never_becomes_external_reference() {
    let parsed = parse_source("f <- function() { x <- 1; x }");
    assert!(!reference_names(&parsed).contains(&"x"));
}

#[test]
fn repeated_predicate_proves_local_binding() {
    let parsed = parse_source(
        "f <- function(alternative) { if (!is.null(alternative)) { tvalue <- 1 }; if (!is.null(alternative)) tvalue }",
    );
    assert!(!reference_names(&parsed).contains(&"tvalue"));
}

#[test]
fn repeated_else_if_predicates_preserve_branch_specific_bindings() {
    let parsed = parse_source(
        r#"f <- function(alternative, prob2) {
            if (alternative == "less") {
                rr <- 1
            } else if (alternative == "greater") {
                rr <- 2
            } else if (alternative == "two.sided") {
                lowerrr <- 3
                upperrr <- 4
            } else {
                stop("bad alternative")
            }
            if (!is.null(prob2)) {
                if (alternative == "less") rr
                else if (alternative == "greater") rr
                else if (alternative == "two.sided") lowerrr + upperrr
            }
        }"#,
    );
    let names = reference_names(&parsed);
    assert!(!names.contains(&"rr"));
    assert!(!names.contains(&"lowerrr"));
    assert!(!names.contains(&"upperrr"));
}

#[test]
fn impure_repeated_predicate_does_not_prove_local_binding() {
    let parsed = parse_source("f <- function() { if (predicate()) x <- 1; if (predicate()) x }");
    let reference = parsed.expressions[0]
        .references
        .iter()
        .find(|reference| reference.name == "x")
        .expect("impure repeated predicate must retain fallthrough");
    assert_eq!(reference.kind, NameRefKind::ConditionalFallthrough);
}

#[test]
fn rebound_predicate_symbol_does_not_prove_local_binding() {
    let parsed =
        parse_source("f <- function(flag) { if (flag) x <- 1; flag <- !flag; if (flag) x }");
    let reference = parsed.expressions[0]
        .references
        .iter()
        .find(|reference| reference.name == "x")
        .expect("rebinding predicate input must retain fallthrough");
    assert_eq!(reference.kind, NameRefKind::ConditionalFallthrough);
}

#[test]
fn broader_later_guard_does_not_hide_real_fallthrough() {
    let parsed = parse_source(
        r#"f <- function(alternative) {
            if (!is.null(alternative)) {
                if (alternative == "less") pvalue <- 1
                else if (alternative == "greater") pvalue <- 2
                else if (alternative == "two.sided") pvalue <- 3
            }
            if (!is.null(alternative)) pvalue
        }"#,
    );
    let reference = parsed.expressions[0]
        .references
        .iter()
        .find(|reference| reference.name == "pvalue")
        .expect("invalid alternative must retain fallthrough");
    assert_eq!(reference.kind, NameRefKind::ConditionalFallthrough);
}

#[test]
fn exhaustive_dispatch_with_stop_proves_local_binding() {
    let parsed = parse_source(
        r#"f <- function(direction) {
            if (direction == "below") showprob <- 1
            else if (direction == "above") showprob <- 2
            else if (direction == "between") showprob <- 3
            else if (direction == "outside") showprob <- 4
            else stop("bad direction")
            showprob
        }"#,
    );
    assert!(!reference_names(&parsed).contains(&"showprob"));
}

#[test]
fn package_local_non_returning_helper_proves_local_binding() {
    let mut context = OakParseContext::default();
    context.non_returning_names.insert(".die".into());
    let parsed = OakParser
        .parse_binding_with_context(
            SourceId(0),
            r#"f <- function(direction) {
                if (direction == "below") showprob <- 1
                else if (direction == "above") showprob <- 2
                else .die()
                showprob
            }"#,
            &context,
        )
        .unwrap();
    assert!(!reference_names(&parsed).contains(&"showprob"));
}

#[test]
fn missing_final_else_keeps_real_fallthrough() {
    let parsed = parse_source(
        r#"f <- function(direction) {
            if (direction == "below") showprob <- 1
            else if (direction == "above") showprob <- 2
            showprob
        }"#,
    );
    let reference = parsed.expressions[0]
        .references
        .iter()
        .find(|reference| reference.name == "showprob")
        .expect("unhandled direction must remain a fallthrough");
    assert_eq!(reference.kind, NameRefKind::ConditionalFallthrough);
}

#[test]
fn zero_iteration_loop_keeps_real_fallthrough() {
    let parsed = parse_source("f <- function(xs) { for (i in seq_along(xs)) sephat <- i; sephat }");
    let reference = parsed.expressions[0]
        .references
        .iter()
        .find(|reference| reference.name == "sephat")
        .expect("zero-iteration loop must retain fallthrough");
    assert_eq!(reference.kind, NameRefKind::ConditionalFallthrough);
}

#[test]
fn locally_shadowed_non_returning_helper_does_not_discharge_fallthrough() {
    let mut context = OakParseContext::default();
    context.non_returning_names.insert(".die".into());
    let parsed = OakParser
        .parse_binding_with_context(
            SourceId(0),
            "f <- function(flag, .die) { if (flag) x <- 1 else .die(); x }",
            &context,
        )
        .unwrap();
    let reference = parsed.expressions[0]
        .references
        .iter()
        .find(|reference| reference.name == "x")
        .expect("shadowed helper may return, so x must still fall through");
    assert_eq!(reference.kind, NameRefKind::ConditionalFallthrough);
}

#[test]
fn locally_shadowed_stop_is_not_summarized_as_non_returning() {
    let context = OakParseContext::default();
    assert!(!closure_definitely_non_returning(
        "function(stop) stop('not base stop')",
        &context,
    ));
}

#[test]
fn non_returning_summary_requires_terminal_stop() {
    let context = OakParseContext::default();
    assert!(closure_definitely_non_returning(
        "function(message) { stop(message) }",
        &context,
    ));
    assert!(!closure_definitely_non_returning(
        "function(flag) { if (flag) stop('bad'); 1 }",
        &context,
    ));
}

#[test]
fn later_formal_is_bound_in_earlier_default() {
    let parsed = parse_source("f <- function(x = y, y = 1) x");
    assert!(!reference_names(&parsed).contains(&"y"));
}

#[test]
fn missing_later_formal_is_still_bound_in_default_environment() {
    let parsed = parse_source("f <- function(x = y, y) x");
    assert!(!reference_names(&parsed).contains(&"y"));
}

#[test]
fn self_referential_default_is_not_global_fallthrough() {
    let parsed = parse_source("f <- function(x = x) x");
    assert!(!reference_names(&parsed).contains(&"x"));
}

#[test]
fn for_variable_is_definitely_bound_inside_body() {
    let parsed = parse_source("f <- function(xs) { for (x in xs) print(x) }");
    assert!(!reference_names(&parsed).contains(&"x"));
}

#[test]
fn for_variable_may_be_unbound_after_zero_iterations() {
    let parsed = parse_source("f <- function(xs) { for (x in xs) {}; print(x) }");
    let reference = parsed.expressions[0]
        .references
        .iter()
        .find(|reference| reference.name == "x")
        .expect("post-loop use must preserve the zero-iteration fallthrough");
    assert_eq!(reference.kind, NameRefKind::ConditionalFallthrough);
}

#[test]
fn preexisting_binding_keeps_post_for_use_bound() {
    let parsed = parse_source("f <- function(xs) { x <- 0; for (x in xs) {}; print(x) }");
    assert!(!reference_names(&parsed).contains(&"x"));
}

#[test]
fn nested_for_variables_are_bound_across_next_paths() {
    let parsed = parse_source(
        "f <- function(xs, ys, flag) { for (x in xs) { for (y in ys) { if (flag) next; print(x + y) } } }",
    );
    let names = reference_names(&parsed);
    assert!(!names.contains(&"x"));
    assert!(!names.contains(&"y"));
}

#[test]
fn captured_activation_superassignment_is_resolved() {
    let parsed = parse_source("outer <- function() { x <- 1; function() { x <<- x + 1 } }");
    let effect = parsed.expressions[0]
        .effects
        .iter()
        .find(|effect| effect.target.as_deref() == Some("x"))
        .expect("superassignment effect");
    assert!(effect.target_enclosing_local);
}

#[test]
fn conditional_outer_assignment_does_not_fake_capture() {
    let parsed = parse_source("outer <- function(flag) { if (flag) x <- 1; function() x <<- 2 }");
    let effect = parsed.expressions[0]
        .effects
        .iter()
        .find(|effect| effect.target.as_deref() == Some("x"))
        .expect("superassignment effect");
    assert!(!effect.target_enclosing_local);
}

#[test]
fn uncaptured_superassignment_remains_unresolved() {
    let parsed = parse_source("outer <- function() { function() x <<- 1 }");
    let effect = parsed.expressions[0]
        .effects
        .iter()
        .find(|effect| effect.target.as_deref() == Some("x"))
        .expect("superassignment effect");
    assert!(!effect.target_enclosing_local);
}

#[test]
fn current_function_local_does_not_satisfy_superassignment() {
    let parsed = parse_source("outer <- function() { x <- 1; x <<- 2 }");
    let effect = parsed.expressions[0]
        .effects
        .iter()
        .find(|effect| effect.target.as_deref() == Some("x"))
        .expect("superassignment effect");
    assert!(!effect.target_enclosing_local);
}

#[test]
fn current_function_parameter_does_not_satisfy_superassignment() {
    let parsed = parse_source("outer <- function(x) { x <<- 2 }");
    let effect = parsed.expressions[0]
        .effects
        .iter()
        .find(|effect| effect.target.as_deref() == Some("x"))
        .expect("superassignment effect");
    assert!(!effect.target_enclosing_local);
}

#[test]
fn enclosing_function_parameter_satisfies_superassignment() {
    let parsed = parse_source("outer <- function(x) { function() x <<- 2 }");
    let effect = parsed.expressions[0]
        .effects
        .iter()
        .find(|effect| effect.target.as_deref() == Some("x"))
        .expect("superassignment effect");
    assert!(effect.target_enclosing_local);
}

#[test]
fn sibling_function_binding_does_not_satisfy_superassignment() {
    let parsed = parse_source(
        "outer <- function() { sibling <- function() { x <- 1 }; function() x <<- 2 }",
    );
    let effect = parsed.expressions[0]
        .effects
        .iter()
        .find(|effect| effect.target.as_deref() == Some("x"))
        .expect("superassignment effect");
    assert!(!effect.target_enclosing_local);
}

#[test]
fn dominating_branch_binding_satisfies_nested_superassignment() {
    let parsed =
        parse_source("outer <- function(flag) { if (flag) { x <- 1; function() x <<- 2 } }");
    let effect = parsed.expressions[0]
        .effects
        .iter()
        .find(|effect| effect.target.as_deref() == Some("x"))
        .expect("superassignment effect");
    assert!(effect.target_enclosing_local);
}

#[test]
fn assignment_dominates_later_use_inside_same_branch() {
    let parsed =
        parse_source("f <- function(flag) { if (flag) { helper <- function() 1; helper() } }");
    assert!(!reference_names(&parsed).contains(&"helper"));
}

#[test]
fn branch_assignment_does_not_dominate_use_after_branch() {
    let parsed = parse_source("f <- function(flag) { if (flag) helper <- function() 1; helper() }");
    assert!(reference_names(&parsed).contains(&"helper"));
}

#[test]
fn local_recursive_closure_sees_its_completed_binding() {
    let parsed = parse_source(
        "f <- function(flag) { if (flag) { recurse <- function(x) if (x) recurse(FALSE); recurse(TRUE) } }",
    );
    assert!(!reference_names(&parsed).contains(&"recurse"));
}

#[test]
fn repeated_boolean_guard_preserves_exhaustive_inner_assignment() {
    let parsed = parse_source(
        "f <- function(enabled, choose_first) { if (enabled) { if (choose_first) value <- 1 else value <- 2 }; if (enabled) print(value) }",
    );
    assert!(!reference_names(&parsed).contains(&"value"));
}

#[test]
fn boolean_alias_correlates_equivalent_null_guard() {
    let parsed = parse_source(
        "f <- function(obj) { present <- !is.null(obj$field); if (!is.null(obj$field)) value <- 1; if (present) print(value) }",
    );
    assert!(!reference_names(&parsed).contains(&"value"));
}

#[test]
fn captured_conditional_binding_is_safe_under_same_stable_guard() {
    let parsed = parse_source(
        "f <- function(enabled, choose_first) { if (enabled) { if (choose_first) callback <- function() 1 else callback <- function() 2 }; invoke <- function() { if (enabled) callback() }; invoke() }",
    );
    assert!(!reference_names(&parsed).contains(&"callback"));
}

#[test]
fn captured_exhaustive_binding_is_safe_under_same_outer_guard() {
    let parsed = parse_source(
        "f <- function(deep, choose_first) { if (deep) { if (choose_first) callback <- function() 1 else callback <- function() 2 }; invoke <- function() { if (deep) callback() }; invoke() }",
    );
    assert!(!reference_names(&parsed).contains(&"callback"));
}

#[test]
fn captured_exhaustive_binding_handles_compound_inner_condition() {
    let parsed = parse_source(
        "f <- function(deep, has_private, candidate) { if (deep) { if (has_private && is.function(candidate)) callback <- candidate else callback <- function() 2 }; invoke <- function() { if (deep) mapply(callback, 1) }; invoke() }",
    );
    assert!(!reference_names(&parsed).contains(&"callback"));
}

#[test]
fn mutated_guard_does_not_validate_captured_conditional_binding() {
    let parsed = parse_source(
        "f <- function(enabled) { if (enabled) callback <- function() 1; invoke <- function() { if (enabled) callback() }; enabled <- !enabled; invoke() }",
    );
    assert!(reference_names(&parsed).contains(&"callback"));
}

#[test]
fn descendant_local_shadow_does_not_mutate_captured_guard() {
    let parsed = parse_source(
        "f <- function(enabled) { if (enabled) callback <- function() 1; shadow <- function() enabled <- FALSE; invoke <- function() { if (enabled) callback() }; invoke() }",
    );
    assert!(!reference_names(&parsed).contains(&"callback"));
}

#[test]
fn rejecting_guard_makes_following_membership_dispatch_exhaustive() {
    let parsed = parse_source(
        r#"f <- function(which, function_value) {
            if (is.null(which) || !(which %in% c("public", "private", "active"))) stop("bad")
            if (which == "public") group <- "public_methods"
            else if (which == "private") group <- "private_methods"
            else if (which == "active") {
                if (function_value) group <- "active" else stop("bad")
            }
            print(group)
        }"#,
    );
    assert!(!reference_names(&parsed).contains(&"group"));
}

#[test]
fn rejecting_guard_handles_value_assignments_in_membership_dispatch() {
    let parsed = parse_source(
        r#"f <- function(which, value) {
            if (is.null(which) || !(which %in% c("public", "private", "active"))) stop("bad")
            if (which == "public") {
                group <- if (is.function(value)) "public_methods" else "public_fields"
            } else if (which == "private") {
                group <- if (is.function(value)) "private_methods" else "private_fields"
            } else if (which == "active") {
                if (is.function(value)) group <- "active" else stop("bad")
            }
            print(group)
        }"#,
    );
    assert!(!reference_names(&parsed).contains(&"group"));
}

#[test]
fn three_level_capture_resolves_outer_activation_binding() {
    let parsed = parse_source(
        "a <- function() { x <- 1; b <- function() { c <- function() x <<- 2; c }; b() }",
    );
    let effect = parsed.expressions[0]
        .effects
        .iter()
        .find(|effect| effect.target.as_deref() == Some("x"))
        .expect("superassignment effect");
    assert!(effect.target_enclosing_local);
}

#[test]
fn language_constants_do_not_become_external_bindings() {
    let parsed = parse_source(
        "f <- function() list(NULL, TRUE, FALSE, NA, NaN, Inf, NA_integer_, NA_real_, NA_complex_, NA_character_)",
    );
    let names = reference_names(&parsed);
    for constant in [
        "NULL",
        "TRUE",
        "FALSE",
        "NA",
        "NaN",
        "Inf",
        "NA_integer_",
        "NA_real_",
        "NA_complex_",
        "NA_character_",
    ] {
        assert!(
            !names.contains(&constant),
            "language constant {constant} leaked as a reference"
        );
    }
}

#[test]
fn call_argument_span_matches_selector_name_reference() {
    let parsed = parse_source("f <- function(x) .Call(.NAME = croot_f, x)");
    let expression = &parsed.expressions[0];
    let reference = expression
        .references
        .iter()
        .find(|reference| reference.name == "croot_f")
        .expect("selector reference");
    let call = expression
        .calls
        .iter()
        .find(|call| call.callee == ".Call")
        .expect("native call");
    let selector = call
        .arg_names
        .iter()
        .position(|name| name.as_deref() == Some(".NAME"))
        .expect("named selector");

    assert_eq!(
        call.args[selector],
        Some(StaticArg::Symbol("croot_f".into()))
    );
    assert_eq!(call.arg_spans[selector].as_ref(), Some(&reference.span));
}

#[test]
fn call_argument_records_definite_local_closure_identity() {
    let parsed = parse_source(
        "f <- function() { callback <- function(x) x; .Call(native_call, 1, callback) }",
    );
    let call = parsed.expressions[0]
        .calls
        .iter()
        .find(|call| call.callee == ".Call")
        .expect("native call");

    assert_eq!(call.local_closure_args, [false, false, true]);
}

#[test]
fn construction_facts_preserve_order_and_target_shapes() {
    let parsed = parse_source(
        "f <- function(template, parent) { env <- new.env(parent = parent); env$self <- env; environment(template) <- env; list2env(template, envir = env) }",
    );
    let construction = &parsed.expressions[0].construction;

    assert_eq!(construction.len(), 4);
    assert!(matches!(
        &construction[0].kind,
        ConstructionExprKind::Assign {
            target: ConstructionTarget::Local { name },
            value,
        } if name == "env" && matches!(
            &value.kind,
            ConstructionExprKind::Call { call } if call.callee == "new.env"
        )
    ));
    assert!(matches!(
        &construction[1].kind,
        ConstructionExprKind::Assign {
            target: ConstructionTarget::Member { name: Some(name), .. },
            ..
        } if name == "self"
    ));
    assert!(matches!(
        &construction[2].kind,
        ConstructionExprKind::Assign {
            target: ConstructionTarget::ClosureEnvironment { .. },
            ..
        }
    ));
    assert!(matches!(
        &construction[3].kind,
        ConstructionExprKind::Call { call } if call.callee == "list2env"
    ));
}

#[test]
fn reenclosure_helper_construction_shape() {
    let parsed = parse_source(
        "assign_func_envs <- function(objs, target_env) { if (is.null(target_env)) return(objs); lapply(objs, function(x) { if (is.function(x)) environment(x) <- target_env; x }) }",
    );
    assert!(matches!(
        &parsed.expressions[0].construction[1].kind,
        ConstructionExprKind::Call { call }
            if call.callee == "lapply"
                && matches!(
                    call.arguments.get(1).and_then(|argument| argument.value.as_ref()).map(|value| &value.kind),
                    Some(ConstructionExprKind::Function { .. })
                )
    ));
}

#[test]
fn exhaustive_equality_chain_correlates_later_else_branch() {
    let parsed = parse_source(
        r#"f <- function(alternative) {
            if (alternative == "less") {
                rr <- 1
            } else if (alternative == "greater") {
                rr <- 2
            } else if (alternative == "two.sided") {
                lowerrr <- 3
                upperrr <- 4
            } else {
                stop("bad alternative")
            }
            if (alternative == "less") rr
            else if (alternative == "greater") rr
            else lowerrr + upperrr
        }"#,
    );
    let names = reference_names(&parsed);
    assert!(!names.contains(&"rr"));
    assert!(!names.contains(&"lowerrr"));
    assert!(!names.contains(&"upperrr"));
}

#[test]
fn non_returning_summary_handles_multi_statement_terminal_stop() {
    let context = OakParseContext::default();
    assert!(closure_definitely_non_returning(
        "function(x) { message <- x; stop(message) }",
        &context,
    ));
}

#[test]
fn non_returning_summary_handles_exhaustive_terminating_if() {
    let context = OakParseContext::default();
    assert!(closure_definitely_non_returning(
        "function(flag) { if (flag) stop('a') else base::stop('b') }",
        &context,
    ));
}

#[test]
fn explicit_return_prevents_never_returns_summary() {
    let context = OakParseContext::default();
    assert!(!closure_definitely_non_returning(
        "function(flag) { if (flag) return(1); stop('otherwise') }",
        &context,
    ));
}

#[test]
fn later_import_from_replaces_an_earlier_one() {
    let mut imports = NamespaceImports::default();
    imports.add_import_from("first".into(), [("target".into(), "first_target".into())]);
    imports.add_import_from("second".into(), [("target".into(), "second_target".into())]);

    assert_eq!(
        imports.resolve("target"),
        NamespaceImportResolution::Imported {
            package: "second".into(),
            binding: "second_target".into(),
            effect_name: "second_target".into(),
        }
    );
}

#[test]
fn later_import_all_replaces_an_earlier_import_from() {
    let mut imports = NamespaceImports::default();
    imports.add_import_from("first".into(), [("target".into(), "target".into())]);
    imports.add_import_all(
        "later".into(),
        Some(BTreeMap::from([
            ("target".into(), "target_impl".into()),
            (".onLoad".into(), ".onLoad".into()),
        ])),
        Vec::new(),
    );

    assert_eq!(
        imports.resolve("target"),
        NamespaceImportResolution::Imported {
            package: "later".into(),
            binding: "target_impl".into(),
            effect_name: "target".into(),
        }
    );
    assert_eq!(
        imports.resolve(".onLoad"),
        NamespaceImportResolution::BaseFallback
    );
    assert_eq!(
        imports
            .names()
            .map(|names| names.into_keys().collect::<Vec<_>>()),
        Ok(vec![BindingName::from("target")])
    );
}

#[test]
fn missing_import_all_blocks_only_names_no_later_import_provides() {
    let mut imports = NamespaceImports::default();
    imports.add_import_all("missing".into(), None, Vec::new());
    imports.add_import_all(
        "later".into(),
        Some(BTreeMap::from([("target".into(), "target".into())])),
        Vec::new(),
    );

    assert_eq!(
        imports.resolve("target"),
        NamespaceImportResolution::Imported {
            package: "later".into(),
            binding: "target".into(),
            effect_name: "target".into(),
        }
    );
    assert_eq!(
        imports.resolve("other"),
        NamespaceImportResolution::MissingImportAll {
            package: "missing".into(),
            binding: "other".into(),
        }
    );
    assert_eq!(imports.names(), Err(&PackageName::from("missing")));
}
