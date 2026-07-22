use std::collections::{HashMap, HashSet, VecDeque};

use indexmap::IndexMap;

use crate::config::{Service, ServiceState};
use crate::error::DependencyError;

/// A node in the dependency graph tracking which services depend on it.
#[derive(Default)]
struct DependencyNode {
    dependents: Vec<String>,
}

/// Recursively collects all service names reachable from the given starting names,
/// including the starting names themselves. Returns an error if any service is
/// unknown, masked, or part of a cyclic dependency.
///
/// Uses a visited set to prevent infinite recursion on cyclic dependencies.
fn collect_dependency_set(
    names: Vec<String>,
    service_map: &IndexMap<String, Service>,
    visited: &mut HashSet<String>,
) -> Result<HashSet<String>, DependencyError> {
    let mut collected = HashSet::new();

    for name in names {
        if visited.contains(&name) {
            continue;
        }
        visited.insert(name.clone());

        let Some(service) = service_map.get(&name) else {
            return Err(DependencyError::UnknownDependency(name));
        };

        if matches!(
            service.state.clone().unwrap_or_default(),
            ServiceState::Masked
        ) {
            return Err(DependencyError::MaskedDepedencies);
        }

        collected.insert(service.name.clone());

        if let Some(ref deps) = service.depends {
            collected.extend(collect_dependency_set(deps.clone(), service_map, visited)?);
        }
    }

    Ok(collected)
}

/// Resolves service dependencies using topological sorting.
///
/// Returns a vector of services in the order they should be started,
/// such that all dependencies of a service come before the service itself.
/// Returns an error if:
/// - A service has an unknown dependency
/// - A service has a masked dependency
/// - There are cyclic dependencies
///
/// # Arguments
///
/// * `start_names` - The names of services to start (does not need to contain dependencies)
/// * `services` - All available services
///
/// # Returns
///
/// A vector of services in dependency order, or an error.
pub fn resolve_dependencies(
    start_names: Vec<String>,
    services: &Vec<Service>,
) -> Result<Vec<Service>, DependencyError> {
    // Turn service list into a look-up map:
    let service_map: IndexMap<String, Service> = services
        .iter()
        .map(|service| (service.name.clone(), service.clone()))
        .collect();

    // Collect all services to be started in the order of the passed `service` vector:
    let dependency_set = collect_dependency_set(start_names, &service_map, &mut HashSet::new())?;
    let services_in_scope: Vec<Service> = service_map
        .iter()
        .filter(|(name, _)| dependency_set.contains(name.as_str()))
        .map(|(_, service)| service.clone())
        .collect();

    // Build graph (DAG) and in degree:
    let mut graph: IndexMap<String, DependencyNode> = services_in_scope
        .iter()
        .map(|service| (service.name.clone(), DependencyNode::default()))
        .collect();
    let mut in_degree: HashMap<String, i32> = services_in_scope
        .iter()
        .map(|service| (service.name.clone(), 0))
        .collect();

    for service in &services_in_scope {
        if let Some(ref deps) = service.depends {
            for dep in deps {
                graph
                    .get_mut(dep)
                    .unwrap()
                    .dependents
                    .push(service.name.clone());
                *in_degree.entry(service.name.clone()).or_insert(0) += 1;
            }
        }
    }

    // Initialize queue with services having no dependencies:
    let mut queue: VecDeque<String> = services_in_scope
        .iter()
        .filter(|service| *in_degree.get(&service.name).unwrap() == 0)
        .map(|service| service.name.clone())
        .collect();

    // Process queue to get topological order from DAG:
    let mut topological_order = Vec::new();
    while let Some(current) = queue.pop_front() {
        topological_order.push(current.clone());

        for dependent in &graph.get(&current).unwrap().dependents {
            let degree = in_degree.get_mut(dependent).unwrap();
            *degree -= 1;
            if *degree == 0 {
                queue.push_back(dependent.clone());
            }
        }
    }

    // Check for cycles:
    if topological_order.len() != services_in_scope.len() {
        return Err(DependencyError::CyclicDepedencies);
    }

    Ok(topological_order
        .into_iter()
        .map(|name| service_map.get(&name).unwrap().clone())
        .collect())
}

#[cfg(test)]
fn build_service(name: &str, dependencies: Vec<&str>, state: Option<ServiceState>) -> Service {
    use crate::config::Command;
    Service {
        name: name.to_string(),
        cmd: Command::String("sh -c ':'".to_string()),
        pwd: None,
        cond: None,
        env: None,
        required: None,
        restart: None,
        wait: None,
        create_window: None,
        state,
        tags: None,
        depends: if dependencies.is_empty() {
            None
        } else {
            Some(dependencies.into_iter().map(|d| d.to_string()).collect())
        },
    }
}

// Keep order of original Vec (esp. if no dependencies given):
#[test]
fn test_resolve_dependencies_1() {
    let services = vec![
        build_service("A", vec![], None),
        build_service("B", vec![], None),
        build_service("C", vec![], None),
        build_service("D", vec![], None),
    ];
    let order = resolve_dependencies(
        vec!["B".to_string(), "D".to_string(), "A".to_string()],
        &services,
    )
    .unwrap();
    assert_eq!(order.len(), 3);
    assert_eq!(order.get(0).unwrap().name, "A");
    assert_eq!(order.get(1).unwrap().name, "B");
    assert_eq!(order.get(2).unwrap().name, "D");
}

// Resolve dependencies:
#[test]
fn test_resolve_dependencies_2() {
    let services = vec![
        build_service("D", vec![], None),
        build_service("C", vec!["A", "B"], None),
        build_service("B", vec!["A"], None),
        build_service("A", vec![], None),
    ];
    let order = resolve_dependencies(vec!["C".to_string()], &services).unwrap();
    assert_eq!(order.len(), 3);
    assert_eq!(order.get(0).unwrap().name, "A");
    assert_eq!(order.get(1).unwrap().name, "B");
    assert_eq!(order.get(2).unwrap().name, "C");
}

// Error on masked dependencies:
#[test]
fn test_resolve_dependencies_3() {
    let services = vec![
        build_service("C", vec!["A", "B"], None),
        build_service("B", vec![], Some(ServiceState::Masked)),
        build_service("A", vec![], None),
    ];
    let result = resolve_dependencies(vec!["C".to_string()], &services);
    assert!(matches!(
        result.unwrap_err(),
        DependencyError::MaskedDepedencies
    ));
}

// Error on cyclic dependencies:
#[test]
fn test_resolve_dependencies_4() {
    let services = vec![
        build_service("C", vec!["B"], None),
        build_service("B", vec!["A"], None),
        build_service("A", vec!["B"], None),
    ];
    let result = resolve_dependencies(vec!["C".to_string()], &services);
    assert!(matches!(
        result.unwrap_err(),
        DependencyError::CyclicDepedencies
    ));
}

// Error on unknown dependencies (name not in list):
#[test]
fn test_resolve_dependencies_5() {
    let services = vec![build_service("A", vec!["B"], None)];
    let result = resolve_dependencies(vec!["A".to_string()], &services);
    assert!(matches!(
        result.unwrap_err(),
        DependencyError::UnknownDependency(_)
    ));
}

// Handle empty start names vec:
#[test]
fn test_resolve_dependencies_6() {
    let services = vec![
        build_service("A", vec![], None),
        build_service("B", vec![], None),
        build_service("C", vec![], None),
    ];
    let order = resolve_dependencies(vec![], &services).unwrap();
    assert_eq!(order.len(), 0);
}

// Handle empty vecs:
#[test]
fn test_resolve_dependencies_7() {
    let services = vec![];
    let order = resolve_dependencies(vec![], &services).unwrap();
    assert_eq!(order.len(), 0);
}
