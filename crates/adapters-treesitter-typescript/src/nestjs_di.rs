use super::model::{NestModule, NestProvider};
use tree_sitter::Node;

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    node.utf8_text(source).ok()
}

fn property<'tree>(object: Node<'tree>, name: &str, source: &[u8]) -> Option<Node<'tree>> {
    let mut cursor = object.walk();
    object
        .named_children(&mut cursor)
        .find(|child| {
            child.kind() == "pair"
                && child
                    .child_by_field_name("key")
                    .and_then(|key| text(key, source))
                    == Some(name)
        })?
        .child_by_field_name("value")
}

fn references(value: Node<'_>, source: &[u8]) -> Vec<String> {
    if value.kind() != "array" {
        return Vec::new();
    }
    let mut cursor = value.walk();
    value
        .named_children(&mut cursor)
        .filter(|child| matches!(child.kind(), "identifier" | "string"))
        .filter_map(|child| text(child, source).map(str::to_owned))
        .collect()
}

fn returned_constructor(node: Node<'_>, source: &[u8]) -> Option<String> {
    if node.kind() == "new_expression" {
        return node
            .child_by_field_name("constructor")
            .and_then(|constructor| text(constructor, source))
            .map(str::to_owned);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| returned_constructor(child, source))
}

fn provider(object: Node<'_>, name: String, source: &[u8]) -> Option<NestProvider> {
    if object.kind() != "object" {
        return None;
    }
    let token = property(object, "provide", source)
        .and_then(|value| text(value, source))?
        .to_owned();
    let existing = property(object, "useExisting", source).is_some();
    let implementation = property(object, "useClass", source)
        .or_else(|| property(object, "useExisting", source))
        .and_then(|value| text(value, source).map(str::to_owned))
        .or_else(|| {
            property(object, "useFactory", source)
                .and_then(|factory| returned_constructor(factory, source))
        })?;
    Some(NestProvider {
        name,
        token,
        implementation,
        existing,
    })
}

fn module(node: Node<'_>, source: &[u8]) -> Option<(NestModule, Vec<NestProvider>)> {
    if node.kind() != "class_declaration" {
        return None;
    }
    let name = node
        .child_by_field_name("name")
        .and_then(|name| text(name, source))?
        .to_owned();
    let mut decorators = node
        .named_children(&mut node.walk())
        .filter(|child| child.kind() == "decorator")
        .collect::<Vec<_>>();
    let mut sibling = node.prev_named_sibling();
    while let Some(decorator) = sibling.filter(|sibling| sibling.kind() == "decorator") {
        decorators.push(decorator);
        sibling = decorator.prev_named_sibling();
    }
    let call = decorators.into_iter().find_map(|decorator| {
        let call = decorator.named_child(0)?;
        (call.kind() == "call_expression"
            && call
                .child_by_field_name("function")
                .and_then(|function| text(function, source))
                == Some("Module"))
        .then_some(call)
    })?;
    let object = call
        .child_by_field_name("arguments")?
        .named_children(&mut call.child_by_field_name("arguments")?.walk())
        .find(|argument| argument.kind() == "object")?;
    let mut inline_providers = Vec::new();
    let mut providers = Vec::new();
    if let Some(array) =
        property(object, "providers", source).filter(|value| value.kind() == "array")
    {
        let mut cursor = array.walk();
        for (index, value) in array.named_children(&mut cursor).enumerate() {
            if matches!(value.kind(), "identifier" | "string") {
                if let Some(reference) = text(value, source) {
                    providers.push(reference.into());
                }
            } else if value.kind() == "object" {
                let provider_name = format!("{name}#provider:{index}");
                if let Some(provider) = provider(value, provider_name.clone(), source) {
                    providers.push(provider_name);
                    inline_providers.push(provider);
                }
            }
        }
    }
    let mut members = providers.clone();
    members.extend(
        property(object, "controllers", source)
            .map(|value| references(value, source))
            .unwrap_or_default(),
    );
    Some((
        NestModule {
            name,
            imports: property(object, "imports", source)
                .map(|value| references(value, source))
                .unwrap_or_default(),
            providers,
            members,
            exports: property(object, "exports", source)
                .map(|value| references(value, source))
                .unwrap_or_default(),
        },
        inline_providers,
    ))
}

pub(super) fn extract(root: Node<'_>, source: &[u8]) -> (Vec<NestModule>, Vec<NestProvider>) {
    let mut modules = Vec::new();
    let mut providers = Vec::new();
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        let declaration = (node.kind() == "export_statement")
            .then(|| node.child_by_field_name("declaration"))
            .flatten();
        if let Some((module, inline_providers)) = module(declaration.unwrap_or(node), source) {
            modules.push(module);
            providers.extend(inline_providers);
        }
        let Some(declarator) = declaration
            .or_else(|| (node.kind() == "lexical_declaration").then_some(node))
            .and_then(|declaration| {
                declaration
                    .named_children(&mut declaration.walk())
                    .find(|child| child.kind() == "variable_declarator")
            })
        else {
            continue;
        };
        let Some((name, object)) = declarator
            .child_by_field_name("name")
            .and_then(|name| text(name, source))
            .zip(declarator.child_by_field_name("value"))
        else {
            continue;
        };
        if let Some(provider) = provider(object, name.into(), source) {
            providers.push(provider);
        }
    }
    (modules, providers)
}
