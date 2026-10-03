use super::census::{Census, node_range};
use super::context::OakParseContext;
use super::proofs::is_base_call;
use super::{LiveCall, static_first_string};
use crate::syntax::facts::{NameRef, PackageGuard, PackageRef, StaticArg, SyntaxEffect};
use crate::syntax::source::TextRange;
use air_r_syntax::AnyRExpression;
use biome_rowan::AstNode;
use oak_semantic::semantic_index::SemanticIndex;

pub(super) fn apply_guard_regions_to_references(
    regions: &[(TextRange, PackageGuard)],
    references: &mut [NameRef],
) {
    for (range, guard) in regions {
        for reference in references.iter_mut() {
            if range.contains_range(reference.span.range()) {
                push_guard(&mut reference.guards, guard.clone());
            }
        }
    }
}

pub(super) fn apply_guard_regions_to_package_refs(
    regions: &[(TextRange, PackageGuard)],
    references: &mut [PackageRef],
) {
    for (range, guard) in regions {
        for reference in references.iter_mut() {
            if range.contains_range(reference.span.range()) {
                push_guard(&mut reference.guards, guard.clone());
            }
        }
    }
}

pub(super) fn apply_guard_regions_to_calls(
    regions: &[(TextRange, PackageGuard)],
    calls: &mut [LiveCall],
) {
    for (range, guard) in regions {
        for call in calls.iter_mut() {
            if range.contains_range(call.site.span.range()) {
                push_guard(&mut call.site.guards, guard.clone());
            }
        }
    }
}

pub(super) fn apply_guard_regions_to_effects(
    regions: &[(TextRange, PackageGuard)],
    effects: &mut [SyntaxEffect],
) {
    for (range, guard) in regions {
        for effect in effects.iter_mut() {
            if range.contains_range(effect.span.range()) {
                push_guard(&mut effect.guards, guard.clone());
            }
        }
    }
}

pub(super) fn if_guard_regions(
    context: &OakParseContext,
    index: &SemanticIndex,
    census: &Census,
    calls: &[LiveCall],
) -> Vec<(TextRange, PackageGuard)> {
    let mut guards = Vec::new();
    for region in &census.ifs {
        let mut required = Vec::new();
        required_calls(&region.condition, context, index, &mut required);
        for range in required {
            let Some(call) = calls
                .iter()
                .find(|call| call.site.span.range() == range && is_base_call(context, &call.site))
            else {
                continue;
            };
            let guard = match call.site.callee.as_str() {
                "requireNamespace" => static_first_string(&call.site)
                    .map(|package| PackageGuard::Available(package.into())),
                "isNamespaceLoaded" => static_first_string(&call.site)
                    .map(|package| PackageGuard::Loaded(package.into())),
                _ => None,
            };
            if let Some(guard) = guard {
                guards.push((region.then_branch, guard));
            }
        }
    }
    guards
}

pub(super) fn hook_guard_regions(
    context: &OakParseContext,
    calls: &[LiveCall],
) -> Vec<(TextRange, PackageGuard)> {
    let mut guards = Vec::new();
    for call in calls {
        if call.site.callee != "setHook" || !is_base_call(context, &call.site) {
            continue;
        }
        let Some(event_argument) = call.raw.args.first() else {
            continue;
        };
        let Some(callback_argument) = call.raw.args.get(1) else {
            continue;
        };
        let Some(event_call) = calls.iter().find(|nested| {
            nested.site.callee == "packageEvent"
                && is_base_call(context, &nested.site)
                && event_argument
                    .value
                    .contains_range(nested.site.span.range())
        }) else {
            continue;
        };
        let Some(package) = static_first_string(&event_call.site) else {
            continue;
        };
        let event = event_call
            .site
            .static_arg(1)
            .and_then(|argument| match argument {
                StaticArg::String(value) => Some(value.as_str()),
                StaticArg::Symbol(_) => None,
            });
        if event == Some("onLoad") {
            guards.push((
                callback_argument.value,
                PackageGuard::Selected(package.into()),
            ));
        }
    }
    guards
}

fn required_calls(
    condition: &AnyRExpression,
    context: &OakParseContext,
    index: &SemanticIndex,
    required: &mut Vec<TextRange>,
) {
    match condition {
        AnyRExpression::RCall(call) => required.push(node_range(call)),
        AnyRExpression::RParenthesizedExpression(parentheses) => {
            if let Ok(body) = parentheses.body() {
                required_calls(&body, context, index, required);
            }
        }
        AnyRExpression::RBinaryExpression(binary) => {
            let Ok(operator) = binary.operator() else {
                return;
            };
            let name = operator.text_trimmed();
            let (scope, _) = index.scope_at(binary.range().start());
            if matches!(name, "&&" | "&")
                && context.resolves_to_base(name)
                && index.resolve(name, scope).is_none()
                && let (Ok(left), Ok(right)) = (binary.left(), binary.right())
            {
                required_calls(&left, context, index, required);
                required_calls(&right, context, index, required);
            }
        }
        _ => {}
    }
}

pub(super) fn push_guard(guards: &mut Vec<PackageGuard>, guard: PackageGuard) {
    if !guards.contains(&guard) {
        guards.push(guard);
    }
}
