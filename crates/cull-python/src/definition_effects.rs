use std::collections::{BTreeMap, BTreeSet};

use cull_core::{
    BindingId, DefId, DefinitionEffectKind, DefinitionKind, DefinitionRole, Diagnostic,
    InternalCandidateDisposition, InternalCandidateInput, InternalCandidateReason,
    InternalCandidateRule, LookupSemantics, ReferenceBindingState, ReferenceId, ReferencePhase,
    ReferenceRole, RemovalRisk, ResidualLookup, SemanticGraphBuilder, UnresolvedReason,
};

pub(crate) fn finalize_definition_effects(
    builder: &mut SemanticGraphBuilder,
    diagnostics: &mut Vec<Diagnostic>,
) {
    derive_definition_effects(builder);
    group_overloads(builder, diagnostics);
    build_internal_candidates(builder);
}

fn derive_definition_effects(builder: &mut SemanticGraphBuilder) {
    let graph = builder.graph().clone();
    for definition in &graph.definitions {
        let mut effects = BTreeSet::new();
        for reference in graph
            .references
            .iter()
            .filter(|reference| reference.module == definition.module)
            .filter(|reference| reference_belongs_to_definition(reference, definition, &graph))
        {
            match reference.role {
                ReferenceRole::Decorator => {
                    effects.insert(DefinitionEffectKind::DecoratorApplication);
                }
                ReferenceRole::DefaultValue => {
                    effects.insert(DefinitionEffectKind::DefaultExpressionEvaluation);
                }
                ReferenceRole::Annotation if reference.phase == ReferencePhase::DefinitionTime => {
                    effects.insert(DefinitionEffectKind::EagerAnnotationEvaluation);
                }
                ReferenceRole::Annotation if reference.phase == ReferencePhase::LazyAnnotation => {
                    effects.insert(DefinitionEffectKind::LazyAnnotationIntrospectionRisk);
                }
                ReferenceRole::BaseClass => {
                    effects.insert(DefinitionEffectKind::ClassBaseEvaluation);
                }
                ReferenceRole::ClassKeyword => {
                    effects.insert(DefinitionEffectKind::ClassKeywordEvaluation);
                }
                ReferenceRole::Metaclass => {
                    effects.insert(DefinitionEffectKind::MetaclassEvaluation);
                }
                _ => {}
            }
        }
        if definition.kind == DefinitionKind::Class {
            effects.insert(DefinitionEffectKind::ClassBodyExecution);
        }

        let effects = effects.into_iter().collect::<Vec<_>>();
        let removal_risk = if effects.is_empty() {
            RemovalRisk::NoKnownDefinitionEffects
        } else {
            let effect_set = builder.intern_definition_effect_set(effects.clone());
            RemovalRisk::Review(effect_set)
        };
        let effect_set = builder.intern_definition_effect_set(effects);
        builder.set_definition_effects(definition.id, effect_set, removal_risk);
    }
}

fn reference_belongs_to_definition(
    reference: &cull_core::ReferenceFact,
    definition: &cull_core::SemanticDefinition,
    graph: &cull_core::SemanticGraph,
) -> bool {
    if reference.span.start >= definition.range.start && reference.span.end <= definition.range.end
    {
        return true;
    }
    if reference.role != ReferenceRole::Decorator
        || reference.span.end > definition.name_range.start
        || definition
            .name_range
            .start
            .saturating_sub(reference.span.end)
            > 256
    {
        return false;
    }
    !graph.definitions.iter().any(|other| {
        other.module == definition.module
            && other.id != definition.id
            && other.name_range.start > reference.span.end
            && other.name_range.start < definition.name_range.start
    })
}

fn group_overloads(builder: &mut SemanticGraphBuilder, diagnostics: &mut Vec<Diagnostic>) {
    let graph = builder.graph().clone();
    let definitions = graph.definitions.clone();
    let binding_scopes = builder
        .graph()
        .bindings
        .iter()
        .map(|binding| (binding.id, binding.scope))
        .collect::<BTreeMap<_, _>>();
    let mut pending: BTreeMap<(cull_core::ScopeId, String), Vec<DefId>> = BTreeMap::new();
    for definition in &definitions {
        let binding_scope = binding_scopes
            .get(&definition.binding)
            .copied()
            .unwrap_or(definition.scope);
        let key = (binding_scope, definition.name.clone());
        if definition.role == DefinitionRole::OverloadDeclaration {
            pending.entry(key).or_default().push(definition.id);
            continue;
        }

        let Some(declarations) = pending.remove(&key) else {
            continue;
        };
        if declarations.is_empty() {
            continue;
        }
        let group = builder.add_overload_group(
            binding_scope,
            definition.name.clone(),
            declarations.clone(),
            Some(definition.id),
        );
        for declaration in declarations {
            builder.set_definition_role(
                declaration,
                DefinitionRole::OverloadDeclaration,
                Some(group),
            );
        }
        builder.set_definition_role(
            definition.id,
            DefinitionRole::OverloadImplementation,
            Some(group),
        );
    }

    for ((scope, name), declarations) in pending {
        if declarations.is_empty() {
            continue;
        }
        let group = builder.add_overload_group(scope, name.clone(), declarations.clone(), None);
        warn_missing_overload_implementation(&graph, diagnostics, &name, &declarations);
        for declaration in declarations {
            builder.set_definition_role(
                declaration,
                DefinitionRole::OverloadDeclaration,
                Some(group),
            );
        }
    }
}

fn warn_missing_overload_implementation(
    graph: &cull_core::SemanticGraph,
    diagnostics: &mut Vec<Diagnostic>,
    overload_name: &str,
    declarations: &[DefId],
) {
    let Some(first_declaration) = declarations
        .iter()
        .filter_map(|declaration| graph.definitions.get(declaration.as_u32() as usize))
        .min_by_key(|definition| (definition.module, definition.name_range.start))
    else {
        return;
    };
    let path = graph
        .modules
        .iter()
        .find(|module| module.id == first_declaration.module)
        .map(|module| module.path.clone());

    let diagnostic = Diagnostic::warning(
        "CULL_P1107",
        format!("overload declarations for `{overload_name}` have no implementation"),
    );
    let diagnostic = if let Some(path) = path {
        diagnostic.with_path(path)
    } else {
        diagnostic
    };
    diagnostics.push(diagnostic.with_range(first_declaration.name_range));
}

fn build_internal_candidates(builder: &mut SemanticGraphBuilder) {
    let graph = builder.graph().clone();
    let mut inbound_by_binding: BTreeMap<BindingId, BTreeSet<ReferenceId>> = BTreeMap::new();
    let mut unsupported_by_binding: BTreeMap<BindingId, BTreeSet<ReferenceId>> = BTreeMap::new();
    let binding_sets = graph
        .binding_sets
        .iter()
        .map(|set| (set.id, set.bindings.clone()))
        .collect::<BTreeMap<_, _>>();
    let flow_uncertainty_sets = graph
        .flow_uncertainty_sets
        .iter()
        .map(|set| (set.id, set.uncertainties.clone()))
        .collect::<BTreeMap<_, _>>();
    let bindings_by_symbol = graph.bindings.iter().fold(
        BTreeMap::<cull_core::SymbolId, Vec<BindingId>>::new(),
        |mut by_symbol, binding| {
            by_symbol
                .entry(binding.symbol)
                .or_default()
                .push(binding.id);
            by_symbol
        },
    );
    let modules_with_unsupported_annotations = graph
        .references
        .iter()
        .filter(|reference| {
            reference.role == ReferenceRole::Annotation
                && matches!(
                    reference.lexical_target,
                    cull_core::Resolution::Unresolved(UnresolvedReason::UnsupportedAnnotation)
                )
        })
        .map(|reference| reference.module)
        .collect::<BTreeSet<_>>();

    for reference in &graph.references {
        match &reference.binding_state {
            ReferenceBindingState::Analyzed(state) => {
                let Some(bindings) = binding_sets.get(&state.bindings) else {
                    continue;
                };
                for binding in bindings {
                    inbound_by_binding
                        .entry(*binding)
                        .or_default()
                        .insert(reference.id);
                }
                if state.residual != ResidualLookup::None
                    || flow_uncertainty_sets
                        .get(&state.uncertainty)
                        .is_some_and(|uncertainties| !uncertainties.is_empty())
                {
                    mark_unsupported_reference(
                        reference,
                        &bindings_by_symbol,
                        &mut unsupported_by_binding,
                    );
                }
            }
            ReferenceBindingState::NotAnalyzed(_) | ReferenceBindingState::NotApplicable => {
                mark_unsupported_reference(
                    reference,
                    &bindings_by_symbol,
                    &mut unsupported_by_binding,
                );
            }
        }
    }

    for definition in &graph.definitions {
        let Some(rule) = candidate_rule(definition.kind) else {
            continue;
        };
        let known_inbound = inbound_by_binding
            .get(&definition.binding)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        let unsupported_inbound = unsupported_by_binding
            .get(&definition.binding)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        let inbound = known_inbound
            .iter()
            .chain(unsupported_inbound.iter())
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let has_module_unsupported_annotation =
            modules_with_unsupported_annotations.contains(&definition.module);
        let mut reasons = BTreeSet::new();
        let disposition = if definition.role == DefinitionRole::OverloadDeclaration {
            reasons.insert(InternalCandidateReason::OverloadDeclaration);
            InternalCandidateDisposition::Suppressed
        } else if !definition.reportable {
            reasons.insert(InternalCandidateReason::NonReportableDefinition);
            InternalCandidateDisposition::Suppressed
        } else if known_inbound.is_empty()
            && unsupported_inbound.is_empty()
            && !has_module_unsupported_annotation
        {
            reasons.insert(InternalCandidateReason::NoSameModuleReferences);
            reasons.insert(InternalCandidateReason::CrossModuleAnalysisDeferred);
            InternalCandidateDisposition::Candidate
        } else {
            if !known_inbound.is_empty() {
                reasons.insert(InternalCandidateReason::HasSameModuleReferences);
            }
            if !unsupported_inbound.is_empty() {
                reasons.insert(InternalCandidateReason::UnresolvedOrUnsupportedReference);
            }
            if has_module_unsupported_annotation {
                reasons.insert(InternalCandidateReason::UnresolvedOrUnsupportedReference);
            }
            InternalCandidateDisposition::Suppressed
        };

        builder.add_internal_candidate(InternalCandidateInput {
            definition: definition.id,
            rule,
            disposition,
            reasons: reasons.into_iter().collect(),
            inbound_references: inbound,
            removal_risk: definition.removal_risk.clone(),
        });
    }
}

fn mark_unsupported_reference(
    reference: &cull_core::ReferenceFact,
    bindings_by_symbol: &BTreeMap<cull_core::SymbolId, Vec<BindingId>>,
    unsupported_by_binding: &mut BTreeMap<BindingId, BTreeSet<ReferenceId>>,
) {
    for symbol in candidate_symbols_for_reference(reference) {
        let Some(bindings) = bindings_by_symbol.get(&symbol) else {
            continue;
        };
        for binding in bindings {
            unsupported_by_binding
                .entry(*binding)
                .or_default()
                .insert(reference.id);
        }
    }
}

fn candidate_symbols_for_reference(
    reference: &cull_core::ReferenceFact,
) -> BTreeSet<cull_core::SymbolId> {
    let mut symbols = BTreeSet::new();
    match &reference.lexical_target {
        cull_core::Resolution::Resolved(symbol) => vec![*symbol],
        cull_core::Resolution::Ambiguous(symbols) => symbols.clone(),
        cull_core::Resolution::External | cull_core::Resolution::Unresolved(_) => Vec::new(),
    }
    .into_iter()
    .for_each(|symbol| {
        symbols.insert(symbol);
    });

    match &reference.lookup {
        LookupSemantics::Direct => {}
        LookupSemantics::GlobalThenBuiltin { global_symbol } => {
            symbols.insert(*global_symbol);
        }
        LookupSemantics::ClassLocalThenGlobalThenBuiltin {
            class_symbol,
            global_symbol,
        } => {
            symbols.insert(*class_symbol);
            symbols.insert(*global_symbol);
        }
    }
    symbols
}

fn candidate_rule(kind: DefinitionKind) -> Option<InternalCandidateRule> {
    match kind {
        DefinitionKind::Function => Some(InternalCandidateRule::UnreferencedFunction),
        DefinitionKind::Class => Some(InternalCandidateRule::UnreferencedClass),
    }
}

#[cfg(test)]
mod tests {
    use cull_core::{
        BindingInput, BindingKind, ContextKind, FileId, FlowFailureReason,
        InternalCandidateDisposition, InternalCandidateReason, LookupSemantics, ModuleId,
        OriginDomain, OriginEvidence, ReferenceBindingState, ReferenceInput, ReferencePhase,
        ReferenceRole, Resolution, ScopeContextInput, ScopeKind, SemanticGraphBuilder,
        SemanticModule, TextRange,
    };

    use super::{candidate_symbols_for_reference, finalize_definition_effects};

    #[test]
    fn class_lookup_candidates_include_global_fallback_symbol() {
        let module = ModuleId::new(0);
        let class_symbol = cull_core::SymbolId::new(1);
        let global_symbol = cull_core::SymbolId::new(2);
        let reference = cull_core::ReferenceFact {
            id: cull_core::ReferenceId::new(0),
            module,
            source_scope: cull_core::ScopeId::new(1),
            source_context: cull_core::ContextId::new(1),
            source_spelling: "target".to_owned(),
            semantic_name: "target".to_owned(),
            lexical_target: Resolution::Resolved(class_symbol),
            lookup: LookupSemantics::ClassLocalThenGlobalThenBuiltin {
                class_symbol,
                global_symbol,
            },
            binding_state: ReferenceBindingState::NotAnalyzed(FlowFailureReason::UnsupportedFlow),
            phase: ReferencePhase::BodyRuntime,
            role: ReferenceRole::Value,
            origin_domain: OriginDomain::Production,
            annotation_semantics: None,
            span: TextRange::new(0, 6),
        };

        let symbols = candidate_symbols_for_reference(&reference);
        assert!(symbols.contains(&class_symbol));
        assert!(symbols.contains(&global_symbol));
    }

    #[test]
    fn unresolved_local_flow_suppresses_internal_candidates() {
        let mut builder = SemanticGraphBuilder::new();
        let module = ModuleId::new(0);
        let file = FileId::new(0);
        let range = TextRange::new(0, 80);
        let (module_scope, module_context) = builder.add_scope_with_context(ScopeContextInput {
            module,
            scope_kind: ScopeKind::Module,
            context_kind: ContextKind::ModuleBody,
            parent_scope: None,
            parent_context: None,
            owner_definition: None,
            name: "pkg.mod".to_owned(),
            range,
        });
        builder.add_module(SemanticModule {
            id: module,
            file,
            name: "pkg.mod".to_owned(),
            path: "pkg/mod.py".to_owned(),
            future_annotations: false,
            origin_domain: OriginDomain::Production,
            origin_evidence: OriginEvidence::DefaultProduction,
            scope: module_scope,
            context: module_context,
        });

        let symbol = builder.symbol(module, module_scope, "target");
        let binding = builder.add_binding(BindingInput {
            module,
            scope: module_scope,
            symbol,
            kind: BindingKind::FunctionDefinition,
            name: "target".to_owned(),
            range: TextRange::new(0, 20),
            name_range: TextRange::new(4, 10),
        });
        let (definition_scope, definition_context) =
            builder.add_scope_with_context(ScopeContextInput {
                module,
                scope_kind: ScopeKind::Function,
                context_kind: ContextKind::FunctionBody,
                parent_scope: Some(module_scope),
                parent_context: Some(module_context),
                owner_definition: None,
                name: "pkg.mod::target".to_owned(),
                range: TextRange::new(0, 20),
            });
        let definition = builder.add_definition(cull_core::DefinitionInput {
            module,
            binding,
            scope: definition_scope,
            context: definition_context,
            kind: cull_core::DefinitionKind::Function,
            name: "target".to_owned(),
            qualified_name: "pkg.mod::target".to_owned(),
            range: TextRange::new(0, 20),
            name_range: TextRange::new(4, 10),
            reportable: true,
            is_async: false,
            origin_domain: OriginDomain::Production,
        });

        builder.add_reference(ReferenceInput {
            module,
            source_scope: module_scope,
            source_context: module_context,
            source_spelling: "target".to_owned(),
            semantic_name: "target".to_owned(),
            lexical_target: Resolution::Resolved(symbol),
            lookup: LookupSemantics::GlobalThenBuiltin {
                global_symbol: symbol,
            },
            binding_state: ReferenceBindingState::NotAnalyzed(FlowFailureReason::UnsupportedFlow),
            phase: ReferencePhase::DefinitionTime,
            role: ReferenceRole::Value,
            origin_domain: OriginDomain::Production,
            annotation_semantics: None,
            span: TextRange::new(40, 46),
        });

        let mut diagnostics = Vec::new();
        finalize_definition_effects(&mut builder, &mut diagnostics);
        let graph = builder.finish();
        let candidate = graph
            .internal_candidates
            .iter()
            .find(|candidate| candidate.definition == definition)
            .expect("missing internal candidate");

        assert_eq!(
            candidate.disposition,
            InternalCandidateDisposition::Suppressed
        );
        assert_eq!(
            candidate.reasons,
            vec![InternalCandidateReason::UnresolvedOrUnsupportedReference]
        );
    }
}
