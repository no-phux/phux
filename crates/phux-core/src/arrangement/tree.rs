//! The split tree each arrangement group holds: [`LayoutNode`], its
//! editing helpers, and stacks of entries sharing one slot.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A node in a binary split tree whose leaves are `L`.
///
/// A `Leaf` holds one `L`; a `Split` divides its rectangle between two
/// children along [`SplitDir`] at `ratio` (the left or top child gets
/// `ratio` of the parent's dimension along the split axis); a `Stack` holds
/// several `L` in one rectangle, showing `active`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutNode<L> {
    /// A single view: the recursion base.
    Leaf(L),
    /// An interior node that splits its rectangle in two.
    Split {
        /// The axis the split is taken along.
        dir: SplitDir,
        /// Fraction of the parent dimension given to `left`, in the open
        /// interval `(0.0, 1.0)` (ADR-0012). [`LayoutNode::split`] and
        /// [`LayoutNode::validate_ratios`] enforce it; the type does not.
        ratio: f32,
        /// Left (for [`SplitDir::Horizontal`]) or top child.
        left: Box<Self>,
        /// Right (for [`SplitDir::Horizontal`]) or bottom child.
        right: Box<Self>,
    },
    /// Two or more entries sharing one rectangle, in strip order. Only
    /// `active` is shown; the others keep running behind it.
    /// [`LayoutNode::validate_ratios`] checks the shape.
    Stack {
        /// Index into `entries` of the one shown.
        active: usize,
        /// The stacked entries, at least two.
        entries: Vec<L>,
    },
}

/// Axis along which a [`LayoutNode::Split`] divides its rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDir {
    /// Side by side (a vertical bar between left and right).
    Horizontal,
    /// Stacked (a horizontal bar between top and bottom).
    Vertical,
}

/// Errors from editing a [`LayoutNode`] tree.
#[derive(Debug, Clone, Copy, PartialEq, Error)]
pub enum LayoutError<L> {
    /// The target leaf is not in the tree.
    #[error("leaf not in layout: {0:?}")]
    PaneNotInLayout(L),
    /// A split ratio is NaN or outside the open interval `(0.0, 1.0)`.
    #[error("invalid split ratio: {0}")]
    InvalidRatio(f32),
    /// A stack has fewer than two entries or `active` past its end.
    #[error("invalid stack")]
    InvalidStack,
}

/// Check that `ratio` lies in the open interval `(0.0, 1.0)`.
///
/// # Errors
///
/// [`LayoutError::InvalidRatio`] for NaN or an out-of-range value.
pub fn validate_ratio<L: std::fmt::Debug>(ratio: f32) -> Result<(), LayoutError<L>> {
    if ratio.is_nan() || ratio <= 0.0 || ratio >= 1.0 {
        Err(LayoutError::InvalidRatio(ratio))
    } else {
        Ok(())
    }
}

impl<L> LayoutNode<L> {
    /// Borrow every leaf in left-to-right (depth-first) order, a stack's
    /// hidden entries included.
    #[must_use]
    pub fn leaves(&self) -> Vec<&L> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves<'a>(&'a self, out: &mut Vec<&'a L>) {
        match self {
            Self::Leaf(l) => out.push(l),
            Self::Split { left, right, .. } => {
                left.collect_leaves(out);
                right.collect_leaves(out);
            }
            Self::Stack { entries, .. } => out.extend(entries),
        }
    }

    /// Borrow the leaves that are shown: every leaf but a stack's hidden
    /// entries, in the same order as [`Self::leaves`].
    #[must_use]
    pub fn shown(&self) -> Vec<&L> {
        match self {
            Self::Leaf(l) => vec![l],
            Self::Split { left, right, .. } => {
                let mut out = left.shown();
                out.extend(right.shown());
                out
            }
            Self::Stack { active, entries } => entries.get(*active).into_iter().collect(),
        }
    }

    /// The same tree shape with every leaf passed through `f`.
    #[must_use]
    pub fn map<M>(&self, f: &mut impl FnMut(&L) -> M) -> LayoutNode<M> {
        match self {
            Self::Leaf(l) => LayoutNode::Leaf(f(l)),
            Self::Split {
                dir,
                ratio,
                left,
                right,
            } => LayoutNode::Split {
                dir: *dir,
                ratio: *ratio,
                left: Box::new(left.map(f)),
                right: Box::new(right.map(f)),
            },
            Self::Stack { active, entries } => LayoutNode::Stack {
                active: *active,
                entries: entries.iter().map(f).collect(),
            },
        }
    }

    /// Like [`Self::map`], stopping at the first leaf `f` rejects.
    pub fn try_map<M, E>(
        &self,
        f: &mut impl FnMut(&L) -> Result<M, E>,
    ) -> Result<LayoutNode<M>, E> {
        Ok(match self {
            Self::Leaf(l) => LayoutNode::Leaf(f(l)?),
            Self::Split {
                dir,
                ratio,
                left,
                right,
            } => LayoutNode::Split {
                dir: *dir,
                ratio: *ratio,
                left: Box::new(left.try_map(f)?),
                right: Box::new(right.try_map(f)?),
            },
            Self::Stack { active, entries } => LayoutNode::Stack {
                active: *active,
                entries: entries.iter().map(&mut *f).collect::<Result<_, E>>()?,
            },
        })
    }

    /// Every stack in the tree, as its shown index and its entries.
    #[must_use]
    pub fn stacks(&self) -> Vec<(usize, &Vec<L>)> {
        match self {
            Self::Leaf(_) => Vec::new(),
            Self::Split { left, right, .. } => {
                let mut out = left.stacks();
                out.extend(right.stacks());
                out
            }
            Self::Stack { active, entries } => vec![(*active, entries)],
        }
    }

    /// The tree with each plain leaf `f` answers for replaced by its answer.
    #[must_use]
    pub fn restack(&self, f: &mut impl FnMut(&L) -> Option<Self>) -> Self
    where
        L: Clone,
    {
        match self {
            Self::Leaf(l) => f(l).unwrap_or_else(|| Self::Leaf(l.clone())),
            Self::Split {
                dir,
                ratio,
                left,
                right,
            } => Self::Split {
                dir: *dir,
                ratio: *ratio,
                left: Box::new(left.restack(f)),
                right: Box::new(right.restack(f)),
            },
            Self::Stack { .. } => self.clone(),
        }
    }

    /// Check every split ratio and stack in the tree.
    ///
    /// # Errors
    ///
    /// [`LayoutError::InvalidRatio`] for the first bad ratio,
    /// [`LayoutError::InvalidStack`] for the first bad stack.
    pub fn validate_ratios(&self) -> Result<(), LayoutError<L>>
    where
        L: std::fmt::Debug,
    {
        match self {
            Self::Leaf(_) => Ok(()),
            Self::Split {
                ratio, left, right, ..
            } => {
                validate_ratio(*ratio)?;
                left.validate_ratios()?;
                right.validate_ratios()
            }
            Self::Stack { active, entries } => {
                if entries.len() < 2 || *active >= entries.len() {
                    return Err(LayoutError::InvalidStack);
                }
                Ok(())
            }
        }
    }
}

impl<L: PartialEq> LayoutNode<L> {
    /// Whether this subtree holds a leaf equal to `target`.
    #[must_use]
    pub fn contains(&self, target: &L) -> bool {
        match self {
            Self::Leaf(l) => l == target,
            Self::Split { left, right, .. } => left.contains(target) || right.contains(target),
            Self::Stack { entries, .. } => entries.contains(target),
        }
    }
}

impl<L: PartialEq + Clone + std::fmt::Debug> LayoutNode<L> {
    /// Split the leaf for `target` into a [`LayoutNode::Split`] whose left
    /// keeps `target` and whose right is a new leaf for `new_leaf`.
    ///
    /// # Errors
    ///
    /// * [`LayoutError::InvalidRatio`] if `ratio` is NaN or outside `(0, 1)`.
    /// * [`LayoutError::PaneNotInLayout`] if `target` is not present.
    ///
    /// The tree is unchanged on error.
    pub fn split(
        &mut self,
        target: &L,
        new_leaf: L,
        dir: SplitDir,
        ratio: f32,
    ) -> Result<(), LayoutError<L>> {
        validate_ratio(ratio)?;
        if !self.contains(target) {
            return Err(LayoutError::PaneNotInLayout(target.clone()));
        }
        self.split_at(target, new_leaf, dir, ratio);
        Ok(())
    }

    fn split_at(&mut self, target: &L, new_leaf: L, dir: SplitDir, ratio: f32) {
        match self {
            Self::Leaf(l) if l == target => {
                let kept = l.clone();
                *self = Self::Split {
                    dir,
                    ratio,
                    left: Box::new(Self::Leaf(kept)),
                    right: Box::new(Self::Leaf(new_leaf)),
                };
            }
            Self::Stack { entries, .. } if entries.contains(target) => {
                let kept = std::mem::replace(self, Self::Leaf(new_leaf.clone()));
                *self = Self::Split {
                    dir,
                    ratio,
                    left: Box::new(kept),
                    right: Box::new(Self::Leaf(new_leaf)),
                };
            }
            Self::Leaf(_) | Self::Stack { .. } => {}
            Self::Split { left, right, .. } => {
                if left.contains(target) {
                    left.split_at(target, new_leaf, dir, ratio);
                } else {
                    right.split_at(target, new_leaf, dir, ratio);
                }
            }
        }
    }
}

/// Remove the leaf for `target` from `tree`, collapsing its parent split so
/// the sibling takes the parent's place. Removing the only leaf leaves
/// `None`.
///
/// # Errors
///
/// [`LayoutError::PaneNotInLayout`] if `target` is not present; `tree` is
/// unchanged.
pub fn remove<L: PartialEq + Clone + std::fmt::Debug>(
    tree: &mut Option<LayoutNode<L>>,
    target: &L,
) -> Result<(), LayoutError<L>> {
    let found = tree.as_ref().is_some_and(|t| t.contains(target));
    if !found {
        return Err(LayoutError::PaneNotInLayout(target.clone()));
    }
    *tree = tree.take().and_then(|t| collapse(t, &|l| l == target));
    Ok(())
}

/// Remove every leaf matching `drop`, collapsing splits as [`remove`] does.
///
/// A stack left with one entry becomes that leaf; one that lost its shown
/// entry shows the entry after it. Returns `None` when no leaf survives.
#[must_use]
pub fn collapse<L>(node: LayoutNode<L>, drop: &impl Fn(&L) -> bool) -> Option<LayoutNode<L>> {
    match node {
        LayoutNode::Leaf(l) => (!drop(&l)).then_some(LayoutNode::Leaf(l)),
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => match (collapse(*left, drop), collapse(*right, drop)) {
            (Some(l), Some(r)) => Some(LayoutNode::Split {
                dir,
                ratio,
                left: Box::new(l),
                right: Box::new(r),
            }),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        },
        LayoutNode::Stack { active, entries } => {
            let before = entries.iter().take(active).filter(|e| drop(e)).count();
            let entries: Vec<L> = entries.into_iter().filter(|e| !drop(e)).collect();
            stack(active.saturating_sub(before), entries)
        }
    }
}

/// `entries` as one slot showing `active` (clamped): `None` for none, a
/// leaf for one, a stack for more.
#[must_use]
pub fn stack<L>(active: usize, mut entries: Vec<L>) -> Option<LayoutNode<L>> {
    match entries.len() {
        0 => None,
        1 => entries.pop().map(LayoutNode::Leaf),
        n => Some(LayoutNode::Stack {
            active: active.min(n - 1),
            entries,
        }),
    }
}
