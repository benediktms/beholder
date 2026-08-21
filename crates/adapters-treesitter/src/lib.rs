use std::{collections::BTreeSet, error::Error, fmt};
use tree_sitter::Node;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorDisposition {
    Report,
    IgnoreKnownGrammarBug,
}

#[derive(Debug, Eq, PartialEq)]
pub enum RecoveryFailure {
    MissingSyntax { lines: Vec<usize> },
}

impl fmt::Display for RecoveryFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSyntax { lines } => {
                write!(
                    formatter,
                    "missing syntax at lines {lines:?} may change nesting"
                )
            }
        }
    }
}

impl Error for RecoveryFailure {}

#[derive(Debug)]
pub struct Recovery<'tree> {
    pub roots: Vec<Node<'tree>>,
    pub error_lines: Vec<usize>,
}

impl Recovery<'_> {
    pub fn is_incomplete(&self) -> bool {
        !self.error_lines.is_empty()
    }
}

pub fn recover(root: Node<'_>) -> Result<Recovery<'_>, RecoveryFailure> {
    recover_with(root, |_| ErrorDisposition::Report)
}

pub fn recover_with<'tree>(
    root: Node<'tree>,
    mut classify_error: impl FnMut(Node<'tree>) -> ErrorDisposition,
) -> Result<Recovery<'tree>, RecoveryFailure> {
    let mut cursor = root.walk();
    let top_level = root.children(&mut cursor).collect::<Vec<_>>();
    let mut stack = vec![(root, None)];
    let mut error_lines = Vec::new();
    let mut missing_lines = Vec::new();
    let mut affected_top_level = BTreeSet::new();

    while let Some((node, top_level_index)) = stack.pop() {
        if node.is_missing() {
            missing_lines.push(node.start_position().row + 1);
            if let Some(index) = top_level_index {
                affected_top_level.insert(index);
            }
        } else if node.is_error() && classify_error(node) == ErrorDisposition::Report {
            error_lines.push(node.start_position().row + 1);
            if let Some(index) = top_level_index {
                affected_top_level.insert(index);
            }
        }

        let mut cursor = node.walk();
        let children = node.children(&mut cursor).collect::<Vec<_>>();
        if node == root {
            stack.extend(
                children
                    .into_iter()
                    .enumerate()
                    .rev()
                    .map(|(index, child)| (child, Some(index))),
            );
        } else {
            stack.extend(
                children
                    .into_iter()
                    .rev()
                    .map(|child| (child, top_level_index)),
            );
        }
    }

    missing_lines.sort_unstable();
    missing_lines.dedup();
    if !missing_lines.is_empty() {
        return Err(RecoveryFailure::MissingSyntax {
            lines: missing_lines,
        });
    }

    error_lines.sort_unstable();
    error_lines.dedup();
    let roots = if error_lines.is_empty() {
        vec![root]
    } else {
        top_level
            .into_iter()
            .enumerate()
            .filter_map(|(index, node)| {
                (node.is_named() && !affected_top_level.contains(&index)).then_some(node)
            })
            .collect()
    };

    Ok(Recovery { roots, error_lines })
}
