/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! CSS table formatting contexts.

#![deny(unsafe_block)]

use block::{BlockFlow, MarginsMayNotCollapse, WidthAndMarginsComputer};
use block::{WidthConstraintInput, WidthConstraintSolution};
use construct::FlowConstructor;
use context::LayoutContext;
use floats::FloatKind;
use flow::{TableFlowClass, FlowClass, Flow, ImmutableFlowUtils};
use fragment::Fragment;
use table_wrapper::{TableLayout, FixedLayout, AutoLayout};
use wrapper::ThreadSafeLayoutNode;

use servo_util::geometry::Au;
use servo_util::geometry;
use std::fmt;
use style::computed_values::table_layout;

/// A table flow corresponded to the table's internal table fragment under a table wrapper flow.
/// The properties `position`, `float`, and `margin-*` are used on the table wrapper fragment,
/// not table fragment per CSS 2.1 § 10.5.
pub struct TableFlow {
    pub block_flow: BlockFlow,

    /// Column widths
    pub col_widths: Vec<Au>,

    /// Column min widths.
    pub col_min_widths: Vec<Au>,

    /// Column pref widths.
    pub col_pref_widths: Vec<Au>,

    /// Table-layout property
    pub table_layout: TableLayout,
}

impl TableFlow {
    pub fn from_node_and_fragment(node: &ThreadSafeLayoutNode,
                                  fragment: Fragment)
                                  -> TableFlow {
        let mut block_flow = BlockFlow::from_node_and_fragment(node, fragment);
        let table_layout = if block_flow.fragment().style().get_table().table_layout ==
                              table_layout::fixed {
            FixedLayout
        } else {
            AutoLayout
        };
        TableFlow {
            block_flow: block_flow,
            col_widths: vec!(),
            col_min_widths: vec!(),
            col_pref_widths: vec!(),
            table_layout: table_layout
        }
    }

    pub fn from_node(constructor: &mut FlowConstructor,
                     node: &ThreadSafeLayoutNode)
                     -> TableFlow {
        let mut block_flow = BlockFlow::from_node(constructor, node);
        let table_layout = if block_flow.fragment().style().get_table().table_layout ==
                              table_layout::fixed {
            FixedLayout
        } else {
            AutoLayout
        };
        TableFlow {
            block_flow: block_flow,
            col_widths: vec!(),
            col_min_widths: vec!(),
            col_pref_widths: vec!(),
            table_layout: table_layout
        }
    }

    pub fn float_from_node(constructor: &mut FlowConstructor,
                           node: &ThreadSafeLayoutNode,
                           float_kind: FloatKind)
                           -> TableFlow {
        let mut block_flow = BlockFlow::float_from_node(constructor, node, float_kind);
        let table_layout = if block_flow.fragment().style().get_table().table_layout ==
                              table_layout::fixed {
            FixedLayout
        } else {
            AutoLayout
        };
        TableFlow {
            block_flow: block_flow,
            col_widths: vec!(),
            col_min_widths: vec!(),
            col_pref_widths: vec!(),
            table_layout: table_layout
        }
    }

    /// Update the corresponding value of self_widths if a value of kid_widths has larger value
    /// than one of self_widths.
    pub fn update_col_widths(self_widths: &mut Vec<Au>, kid_widths: &Vec<Au>) -> Au {
        let mut sum_widths = Au(0);
        let mut kid_widths_it = kid_widths.iter();
        for self_width in self_widths.mut_iter() {
            match kid_widths_it.next() {
                Some(kid_width) => {
                    if *self_width < *kid_width {
                        *self_width = *kid_width;
                    }
                },
                None => {}
            }
            sum_widths = sum_widths + *self_width;
        }
        sum_widths
    }

    /// Assign height for table flow.
    ///
    /// TODO(#2014, pcwalton): This probably doesn't handle margin collapse right.
    ///
    /// inline(always) because this is only ever called by in-order or non-in-order top-level
    /// methods
    #[inline(always)]
    fn assign_height_table_base(&mut self, layout_context: &mut LayoutContext) {
        self.block_flow.assign_height_block_base(layout_context, MarginsMayNotCollapse);
    }

    pub fn build_display_list_table(&mut self, layout_context: &LayoutContext) {
        debug!("build_display_list_table: same process as block flow");
        self.block_flow.build_display_list_block(layout_context);
    }
}

impl Flow for TableFlow {
    fn class(&self) -> FlowClass {
        TableFlowClass
    }

    fn as_table<'a>(&'a mut self) -> &'a mut TableFlow {
        self
    }

    fn as_block<'a>(&'a mut self) -> &'a mut BlockFlow {
        &mut self.block_flow
    }

    fn col_widths<'a>(&'a mut self) -> &'a mut Vec<Au> {
        &mut self.col_widths
    }

    fn col_min_widths<'a>(&'a self) -> &'a Vec<Au> {
        &self.col_min_widths
    }

    fn col_pref_widths<'a>(&'a self) -> &'a Vec<Au> {
        &self.col_pref_widths
    }

    /// The specified column widths are set from column group and the first row for the fixed
    /// table layout calculation.
    /// The maximum min/pref widths of each column are set from the rows for the automatic
    /// table layout calculation.
    fn bubble_widths(&mut self, _: &mut LayoutContext) {
        let mut min_width = Au(0);
        let mut pref_width = Au(0);
        let mut did_first_row = false;

        for kid in self.block_flow.base.child_iter() {
            assert!(kid.is_proper_table_child());

            if kid.is_table_colgroup() {
                self.col_widths.push_all(kid.as_table_colgroup().widths.as_slice());
                self.col_min_widths = self.col_widths.clone();
                self.col_pref_widths = self.col_widths.clone();
            } else if kid.is_table_rowgroup() || kid.is_table_row() {
                // read column widths from table-row-group/table-row, and assign
                // width=0 for the columns not defined in column-group
                // FIXME: need to read widths from either table-header-group OR
                // first table-row
                match self.table_layout {
                    FixedLayout => {
                        let kid_col_widths = kid.col_widths();
                        if !did_first_row {
                            did_first_row = true;
                            let mut child_widths = kid_col_widths.iter();
                            for col_width in self.col_widths.mut_iter() {
                                match child_widths.next() {
                                    Some(child_width) => {
                                        if *col_width == Au::new(0) {
                                            *col_width = *child_width;
                                        }
                                    },
                                    None => break
                                }
                            }
                        }
                        let num_child_cols = kid_col_widths.len();
                        let num_cols = self.col_widths.len();
                        debug!("table until the previous row has {} column(s) and this row has {} column(s)",
                               num_cols, num_child_cols);
                        for i in range(num_cols, num_child_cols) {
                            self.col_widths.push( *kid_col_widths.get(i) );
                        }
                    },
                    AutoLayout => {
                        min_width = TableFlow::update_col_widths(&mut self.col_min_widths, kid.col_min_widths());
                        pref_width = TableFlow::update_col_widths(&mut self.col_pref_widths, kid.col_pref_widths());

                        // update the number of column widths from table-rows.
                        let num_cols = self.col_min_widths.len();
                        let num_child_cols = kid.col_min_widths().len();
                        debug!("table until the previous row has {} column(s) and this row has {} column(s)",
                               num_cols, num_child_cols);
                        for i in range(num_cols, num_child_cols) {
                            self.col_widths.push(Au::new(0));
                            let new_kid_min = *kid.col_min_widths().get(i);
                            self.col_min_widths.push( new_kid_min );
                            let new_kid_pref = *kid.col_pref_widths().get(i);
                            self.col_pref_widths.push( new_kid_pref );
                            min_width = min_width + new_kid_min;
                            pref_width = pref_width + new_kid_pref;
                        }
                    }
                }
            }
        }
        self.block_flow.base.intrinsic_widths.minimum_width = min_width;
        self.block_flow.base.intrinsic_widths.preferred_width =
            geometry::max(min_width, pref_width);
    }

    /// Recursively (top-down) determines the actual width of child contexts and fragments. When
    /// called on this context, the context has had its width set by the parent context.
    fn assign_widths(&mut self, ctx: &mut LayoutContext) {
        debug!("assign_widths({}): assigning width for flow", "table");

        // The position was set to the containing block by the flow's parent.
        let containing_block_width = self.block_flow.base.position.size.width;

        let mut num_unspecified_widths = 0;
        let mut total_column_width = Au::new(0);
        for col_width in self.col_widths.iter() {
            if *col_width == Au::new(0) {
                num_unspecified_widths += 1;
            } else {
                total_column_width = total_column_width.add(col_width);
            }
        }

        let width_computer = InternalTable;
        width_computer.compute_used_width(&mut self.block_flow, ctx, containing_block_width);

        let left_content_edge = self.block_flow.fragment.border_padding.left;
        let padding_and_borders = self.block_flow.fragment.border_padding.horizontal();
        let content_width = self.block_flow.fragment.border_box.size.width - padding_and_borders;

        match self.table_layout {
            FixedLayout => {
                // In fixed table layout, we distribute extra space among the unspecified columns if there are
                // any, or among all the columns if all are specified.
                if (total_column_width < content_width) && (num_unspecified_widths == 0) {
                    let ratio = content_width.to_f64().unwrap() / total_column_width.to_f64().unwrap();
                    for col_width in self.col_widths.mut_iter() {
                        *col_width = (*col_width).scale_by(ratio);
                    }
                } else if num_unspecified_widths != 0 {
                    let extra_column_width = (content_width - total_column_width) / Au::new(num_unspecified_widths);
                    for col_width in self.col_widths.mut_iter() {
                        if *col_width == Au(0) {
                            *col_width = extra_column_width;
                        }
                    }
                }
            }
            _ => {}
        }

        self.block_flow.propagate_assigned_width_to_children(left_content_edge, content_width, Some(self.col_widths.clone()));
    }

    fn assign_height(&mut self, ctx: &mut LayoutContext) {
        debug!("assign_height: assigning height for table");
        self.assign_height_table_base(ctx);
    }

    fn compute_absolute_position(&mut self) {
        self.block_flow.compute_absolute_position()
    }
}

impl fmt::Show for TableFlow {
    /// Outputs a debugging string describing this table flow.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "TableFlow: {}", self.block_flow)
    }
}

/// Table, TableRowGroup, TableRow, TableCell types.
/// Their widths are calculated in the same way and do not have margins.
pub struct InternalTable;

impl WidthAndMarginsComputer for InternalTable {
    /// Compute the used value of width, taking care of min-width and max-width.
    ///
    /// CSS Section 10.4: Minimum and Maximum widths
    fn compute_used_width(&self,
                          block: &mut BlockFlow,
                          ctx: &mut LayoutContext,
                          parent_flow_width: Au) {
        let input = self.compute_width_constraint_inputs(block, parent_flow_width, ctx);
        let solution = self.solve_width_constraints(block, &input);
        self.set_width_constraint_solutions(block, solution);
    }

    /// Solve the width and margins constraints for this block flow.
    fn solve_width_constraints(&self, _: &mut BlockFlow, input: &WidthConstraintInput)
                               -> WidthConstraintSolution {
        WidthConstraintSolution::new(input.available_width, Au::new(0), Au::new(0))
    }
}
