use phux_core::arrangement::{LayoutNode, SplitDir};

use super::{decode_row, stored_json};

#[test]
fn a_window_past_the_json_recursion_limit_saves_and_reads_back() {
    let mut tree = LayoutNode::Leaf(0_u32);
    for leaf in 1..200 {
        tree = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutNode::Leaf(leaf)),
            right: Box::new(tree),
        };
    }
    let encoded = stored_json(&tree).expect("a deep window saves");
    assert_eq!(decode_row::<LayoutNode<u32>>(&encoded).unwrap(), tree);
}
