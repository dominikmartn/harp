use std::collections::HashSet;

use itertools::{Either, Itertools};
use warp_core::features::FeatureFlag;
use warpui::{EntityId, UpdateView, ViewContext};

use super::{Workspace, group_member_indices};
use crate::menu::{MenuItem, MenuItemFields};
use crate::tab::{MOVE_TO_GROUP_LABEL, TabData};
use crate::workspace::action::{TabContextMenuAnchor, WorkspaceAction};
use crate::workspace::tab_group::{TabGroup, TabGroupId};
#[cfg(not(target_family = "wasm"))]
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
#[cfg(not(target_family = "wasm"))]
use crate::workspace::tab_settings::TabSettings;
#[cfg(not(target_family = "wasm"))]
use settings::Setting as _;
#[cfg(not(target_family = "wasm"))]
use warpui::SingletonEntity as _;
use crate::workspace::util::PaneViewLocator;

// TODO(johnturcoo) move tab grouping helpers here from workspace/view.rs.
impl Workspace {
    /// Clears the multi-selection on every tab.
    pub(super) fn clear_tab_multi_selection(&mut self, ctx: &mut ViewContext<Self>) {
        for tab in &mut self.tabs {
            tab.in_multi_selection = false;
        }
        ctx.notify();
    }

    /// Adds the inclusive range between `anchor_index` and `clicked_index` to
    /// the multi-selection, expanding any collapsed groups the range crosses.
    /// Existing multi-selection outside the range is preserved (additive
    /// semantics), so cmd-click selections survive a subsequent shift-click.
    fn set_tab_range_selection(
        &mut self,
        anchor_index: usize,
        clicked_index: usize,
        ctx: &mut ViewContext<Self>,
    ) {
        // Determine the bounds for our range selection.
        let lo_index = anchor_index.min(clicked_index);
        let hi_index = anchor_index.max(clicked_index);

        // Identify groups in the selection range.
        let crossed_group_ids: HashSet<TabGroupId> = self
            .tabs
            .get(lo_index..=hi_index)
            .into_iter()
            .flatten()
            .filter_map(|tab| tab.group_id)
            .collect();

        // Expand any groups within the selected range, so user can see what they are selecting.
        for group_id in &crossed_group_ids {
            self.expand_tab_group(*group_id, ctx);
        }

        // Add tabs in the selected range to the multi-selection.
        self.tabs
            .iter_mut()
            .enumerate()
            .filter(|(index, _)| (lo_index..=hi_index).contains(index))
            .for_each(|(_, tab)| tab.in_multi_selection = true);

        ctx.dispatch_global_action("workspace:save_app", ());
        ctx.notify();
    }

    /// Shift-click on a vertical tab row: selects every tab between the
    /// active tab and `locator` (inclusive).
    pub(super) fn shift_select_tab_range(
        &mut self,
        locator: PaneViewLocator,
        ctx: &mut ViewContext<Self>,
    ) {
        if !FeatureFlag::GroupedTabs.is_enabled() {
            return;
        }
        // Identify index of the tab that was shift-clicked.
        if let Some(clicked_index) = self
            .tabs
            .iter()
            .position(|tab| tab.pane_group.id() == locator.pane_group_id)
        {
            self.set_tab_range_selection(self.active_tab_index, clicked_index, ctx);
        }
    }

    /// Cmd-click on a tab: toggles the multi-selection flag
    /// for a single tab.
    pub(super) fn toggle_tab_multi_selection(
        &mut self,
        locator: PaneViewLocator,
        ctx: &mut ViewContext<Self>,
    ) {
        if !FeatureFlag::GroupedTabs.is_enabled() {
            return;
        }
        if let Some(tab) = self
            .tabs
            .iter_mut()
            .find(|tab| tab.pane_group.id() == locator.pane_group_id)
        {
            // Toggle multi selection flag for this tab.
            tab.in_multi_selection = !tab.in_multi_selection;
            ctx.notify();
        }
    }

    /// Returns all tabs that are part of the multi tab selection.
    /// The active tab index is always included if any other tab is marked
    /// as selected. This is to handle the edge case where we only mark other
    /// tabs as selected via command click.
    pub(super) fn selected_tab_indices(&self) -> Vec<usize> {
        let any_flagged = self.tabs.iter().any(|tab| tab.in_multi_selection);
        // If no tab is part of the multi selection, return empty list.
        if !any_flagged {
            return Vec::new();
        }
        // Otherwise, the active tab must always be part of the multi tab selection.
        self.tabs
            .iter()
            .enumerate()
            .filter(|(index, tab)| tab.in_multi_selection || *index == self.active_tab_index)
            .map(|(index, _)| index)
            .collect()
    }

    /// Drives right-click menu dispatch: when a selected tab is right-clicked
    /// and the selection covers multiple tabs, show the multi-tab menu;
    /// otherwise fall through to the normal single-pane menu.
    pub(super) fn is_tab_in_multi_tab_selection(&self, tab_index: usize) -> bool {
        if !FeatureFlag::GroupedTabs.is_enabled() {
            return false;
        }
        let indices = self.selected_tab_indices();
        indices.len() > 1 && indices.contains(&tab_index)
    }

    /// Gates the "Remove from group" menu item. All selected tabs
    /// must be in the same group in order to display this option.
    pub(super) fn selection_shared_group(&self) -> Option<TabGroupId> {
        let indices = self.selected_tab_indices();
        let mut group_ids = indices
            .iter()
            .filter_map(|index| self.tabs.get(*index))
            .map(|tab| tab.group_id);
        let first = group_ids.next()??;
        group_ids.all(|gid| gid == Some(first)).then_some(first)
    }

    /// Re-seats `active_tab_index` so the previously-active pane group stays
    /// visually active across a tab reorder. Pass the pane group id captured
    /// before the reorder; no-op if it can't be found.
    pub(super) fn restore_active_tab_index(&mut self, pane_group_id: Option<EntityId>) {
        if let Some(active_id) = pane_group_id
            && let Some(new_index) = self
                .tabs
                .iter()
                .position(|tab| tab.pane_group.id() == active_id)
        {
            self.active_tab_index = new_index;
        }
    }

    /// Re-anchors a freshly restored block of tabs past `host_group_id`'s last
    /// member, so restoring into a window whose active tab was already grouped
    /// does not leave that group split around the new tabs. `restored_indices`
    /// indexes the current tab list and must be read before any other move.
    ///
    /// A no-op while the host group is still contiguous: `NewTabPlacement` can
    /// land the block outside the group entirely, and that placement is the
    /// user's setting, not something to override.
    pub(super) fn move_restored_block_past_group(
        &mut self,
        restored_indices: &[usize],
        host_group_id: TabGroupId,
    ) {
        if restored_indices.is_empty() {
            return;
        }

        // Restored tabs never join the host group -- their ids are minted
        // fresh -- so these are exactly the pre-existing members.
        let member_indices: Vec<usize> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| tab.group_id == Some(host_group_id))
            .map(|(index, _)| index)
            .collect();
        let (Some(&first), Some(&last)) = (member_indices.first(), member_indices.last()) else {
            return;
        };
        if last - first + 1 == member_indices.len() {
            return;
        }

        let restored: HashSet<usize> = restored_indices.iter().copied().collect();
        let active_pane_group_id = self
            .tabs
            .get(self.active_tab_index)
            .map(|tab| tab.pane_group.id());

        let mut restored_tabs = Vec::with_capacity(restored.len());
        let mut other_tabs = Vec::with_capacity(self.tabs.len());
        for (index, tab) in self.tabs.drain(..).enumerate() {
            if restored.contains(&index) {
                restored_tabs.push(tab);
            } else {
                other_tabs.push(tab);
            }
        }

        // Mirrors `new_tab_group_from_selected_tabs`: search from the right for
        // the host group's last surviving member and insert just past it.
        let insert_at = other_tabs
            .iter()
            .rposition(|tab| tab.group_id == Some(host_group_id))
            .map_or(other_tabs.len(), |last| last + 1);
        other_tabs.splice(insert_at..insert_at, restored_tabs);
        self.tabs = other_tabs;

        self.restore_active_tab_index(active_pane_group_id);
    }

    /// Context-aware "create group" entry point used by the
    /// `workspace:new_tab_group_from_active_or_selected_tabs` keybinding. When
    /// the multi-selection covers 2+ tabs, groups the selection; otherwise
    /// groups just the active tab. `selected_tab_indices` already folds the
    /// active tab into the selection, so a lone flagged active tab (or no
    /// selection at all) takes the single-tab path.
    pub(super) fn new_tab_group_from_active_or_selected_tabs(
        &mut self,
        ctx: &mut ViewContext<Self>,
    ) {
        if !FeatureFlag::GroupedTabs.is_enabled() {
            return;
        }
        if self.selected_tab_indices().len() >= 2 {
            self.new_tab_group_from_selected_tabs(ctx);
        } else {
            self.new_tab_group_from_tab(self.active_tab_index, ctx);
        }
    }

    /// Context-aware "remove from group" entry point used by the
    /// `workspace:remove_active_or_selected_tabs_from_group` keybinding. With a
    /// 2+ multi-selection, removes the whole selection from its group;
    /// otherwise removes just the active tab.
    pub(super) fn remove_active_or_selected_tabs_from_group(
        &mut self,
        ctx: &mut ViewContext<Self>,
    ) {
        if !FeatureFlag::GroupedTabs.is_enabled() {
            return;
        }
        if self.selected_tab_indices().len() >= 2 {
            self.remove_selected_tabs_from_group(ctx);
        } else {
            self.remove_tab_from_group(self.active_tab_index, ctx);
        }
    }

    /// "Create group from tabs" menu action. Group membership requires
    /// tabs to be contiguous in the bar, so we gather the selected tabs into
    /// a single block anchored at the earliest selected tab's position before
    /// binding them to the new group. When that earliest tab was itself in a
    /// group, the block is placed just past that group's last remaining
    /// member so the existing group stays contiguous instead of being split.
    pub(super) fn new_tab_group_from_selected_tabs(&mut self, ctx: &mut ViewContext<Self>) {
        if !FeatureFlag::GroupedTabs.is_enabled() {
            return;
        }
        let selected_indices = self.selected_tab_indices();

        // Should be unreachable: the multi-tab menu only opens when 2+ tabs
        // are selected.
        if selected_indices.len() < 2 {
            log::warn!(
                "new_tab_group_from_selected_tabs called with {} selected tab(s); expected at least 2",
                selected_indices.len()
            );
            return;
        }

        // Remember the groups the selected tabs are leaving so we can prune
        // any that become empty after the move.
        let previous_group_ids: HashSet<TabGroupId> = selected_indices
            .iter()
            .filter_map(|index| self.tabs[*index].group_id)
            .collect();

        let group = TabGroup::new();
        let group_id = group.id;
        self.tab_groups.insert(group_id, group);

        // Store the active tab (pane group).
        let active_pane_group_id = self
            .tabs
            .get(self.active_tab_index)
            .map(|tab| tab.pane_group.id());

        // Anchor the group block at the earliest selected tab. `selected_indices`
        // is ascending, so its first entry is the earliest tab in the list, and
        // we remember the group it currently belongs to (if any).
        let anchor_index = selected_indices[0];
        let anchor_previous_group_id = self.tabs[anchor_index].group_id;

        // Assign membership and clear flags for every selected tab. The new
        // group is unpinned, so any selected tab in set as unpinned.
        for &index in &selected_indices {
            let tab = &mut self.tabs[index];
            tab.group_id = Some(group_id);
            tab.pinned = false;
            tab.in_multi_selection = false;
        }

        // Split tabs into the new group's members and all other tabs.
        let (selected_tabs, mut other_tabs): (Vec<_>, Vec<_>) = self
            .tabs
            .drain(..)
            .partition(|tab| tab.group_id == Some(group_id));

        // Compute where to splice the new group block into `other_tabs`.
        //
        // Simple case — anchor tab was NOT in a group:
        //   Every tab before `anchor_index` in the original list was unselected
        //   (because `anchor_index` is the smallest selected index), so those
        //   tabs are still at the front of `other_tabs` in their original order.
        //   Inserting at `anchor_index` in `other_tabs` places the block exactly
        //   where the anchor tab used to be.
        //
        // Edge case — anchor tab WAS in an existing group G:
        //   Other surviving members of G are still in `other_tabs`. Inserting at
        //   `anchor_index` could land in the middle of G's run and split it.
        //   Example: tabs = [A(G), B(G), C(G), D] and we select B and D.
        //   other_tabs = [A(G), C(G), D]. anchor_index = 1, which points at C —
        //   inserting there would produce [A(G), B(new), D(new), C(G)], breaking
        //   G's contiguity. Instead we search other_tabs from the right for the
        //   last surviving G member (C, at index 1) and insert after it (index 2),
        //   giving [A(G), C(G), B(new), D(new)].
        let insert_at = anchor_previous_group_id
            .and_then(|prev_group_id| {
                other_tabs
                    .iter()
                    .rposition(|tab| tab.group_id == Some(prev_group_id))
                    .map(|last| last + 1)
            })
            .unwrap_or(anchor_index);

        // Our insertion index for this group should be below any pinned items.
        let insert_at = self.clamp_to_unpinned_region(&other_tabs, insert_at);

        other_tabs.splice(insert_at..insert_at, selected_tabs);
        self.tabs = other_tabs;

        self.restore_active_tab_index(active_pane_group_id);

        // Prune any groups that are now empty.
        for previous_group_id in previous_group_ids {
            self.prune_empty_tab_group(previous_group_id, ctx);
        }

        ctx.dispatch_global_action("workspace:save_app", ());
        ctx.notify();

        ctx.dispatch_typed_action_deferred(WorkspaceAction::RenameTabGroup(group_id));
    }

    /// "Move to group" menu action. The destination group's first-member
    /// position is preserved so the group doesn't visually jump while the
    /// selected tabs are folded in.
    pub(super) fn move_selected_tabs_to_group(
        &mut self,
        group_id: TabGroupId,
        ctx: &mut ViewContext<Self>,
    ) {
        if !FeatureFlag::GroupedTabs.is_enabled() || !self.tab_groups.contains_key(&group_id) {
            return;
        }
        let selected_indices = self.selected_tab_indices();

        // Should be unreachable: the multi-tab menu only opens when 2+ tabs
        // are selected.
        if selected_indices.len() < 2 {
            log::warn!(
                "move_selected_tabs_to_group called with {} selected tab(s); expected at least 2",
                selected_indices.len()
            );
            return;
        }

        // Store all groups that tabs previously belonged to, excluding the
        // group that we are moving tabs to. In order to prune these groups later.
        let previous_group_ids: HashSet<TabGroupId> = selected_indices
            .iter()
            .filter_map(|index| self.tabs[*index].group_id)
            .filter(|gid| *gid != group_id)
            .collect();

        // Anchor the block at the existing first member so the group doesn't jump.
        let first_existing_member = self
            .tabs
            .iter()
            .position(|tab| tab.group_id == Some(group_id));

        // Store the active tab (pane group).
        let active_pane_group_id = self
            .tabs
            .get(self.active_tab_index)
            .map(|tab| tab.pane_group.id());

        // Assign membership and clear flags for every selected tab. Entering
        // the group removes any per-tab pinned flag — the destination group's
        // own `pinned` flag now governs the member's position.
        for &index in &selected_indices {
            let tab = &mut self.tabs[index];
            tab.group_id = Some(group_id);
            tab.pinned = false;
            tab.in_multi_selection = false;
        }

        // Anchor the group block at its original first-member position, shifted
        // left by the count of newly-added members from before that position.
        let insert_at = first_existing_member.map_or(0, |first| {
            first - selected_indices.iter().filter(|&&i| i < first).count()
        });
        // Split tabs into the destination group's members (existing + newly
        // added) and the rest.
        let (members, mut rest): (Vec<_>, Vec<_>) = self
            .tabs
            .drain(..)
            .partition(|tab| tab.group_id == Some(group_id));
        // Drop the group block into rest at the anchored position.
        rest.splice(insert_at..insert_at, members);
        self.tabs = rest;

        self.restore_active_tab_index(active_pane_group_id);

        self.expand_tab_group(group_id, ctx);

        // Prune any groups that are now empty.
        for previous_group_id in previous_group_ids {
            self.prune_empty_tab_group(previous_group_id, ctx);
        }

        ctx.dispatch_global_action("workspace:save_app", ());
        ctx.notify();
    }

    /// "Remove from group" menu action. Removed tabs land just below the
    /// group's remaining members so the user can still see where they came
    /// from; if the group ends up empty it's pruned and the removed block
    /// anchors at the original position instead.
    pub(super) fn remove_selected_tabs_from_group(&mut self, ctx: &mut ViewContext<Self>) {
        if !FeatureFlag::GroupedTabs.is_enabled() {
            return;
        }
        let Some(group_id) = self.selection_shared_group() else {
            // Only a single-group selection has an unambiguous group to leave.
            self.clear_tab_multi_selection(ctx);
            return;
        };

        // Capture the group's first index before clearing membership so we can
        // anchor the removed block if the group ends up empty.
        let group_first_index = self
            .tabs
            .iter()
            .position(|tab| tab.group_id == Some(group_id))
            .unwrap_or(0);
        // Store the active tab (pane group).
        let active_pane_group_id = self
            .tabs
            .get(self.active_tab_index)
            .map(|tab| tab.pane_group.id());
        let selected_indices = self.selected_tab_indices();
        let selected_set: HashSet<usize> = selected_indices.iter().copied().collect();

        // Clear the group that all selected tabs belonged to.
        for &index in &selected_indices {
            self.tabs[index].group_id = None;
        }

        // Non-selected tabs originally before the group's first member; if the
        // group ends up empty we fall back to inserting at this position.
        let kept_before_group = group_first_index
            - selected_indices
                .iter()
                .filter(|&&i| i < group_first_index)
                .count();

        // Split tabs by index into the removed (selected) block and the rest.
        let (removed, mut rest): (Vec<_>, Vec<_>) =
            self.tabs
                .drain(..)
                .enumerate()
                .partition_map(|(index, tab)| {
                    if selected_set.contains(&index) {
                        Either::Left(tab)
                    } else {
                        Either::Right(tab)
                    }
                });
        // Anchor the removed block just after the group's remaining members;
        // if none remain, fall back to the pre-computed prefix position.
        let natural_insert_at = match rest.iter().rposition(|tab| tab.group_id == Some(group_id)) {
            Some(last) => last + 1,
            None => kept_before_group,
        };
        // The removed tabs are now unpinned (they left a possibly-pinned
        // group); they must land past every effectively pinned tab in
        // not just past the source group's remaining members.
        let insert_at = self.clamp_to_unpinned_region(&rest, natural_insert_at);
        rest.splice(insert_at..insert_at, removed);
        self.tabs = rest;

        self.clear_tab_multi_selection(ctx);
        self.restore_active_tab_index(active_pane_group_id);
        self.prune_empty_tab_group(group_id, ctx);

        ctx.dispatch_global_action("workspace:save_app", ());
        ctx.notify();
    }

    /// Items shown in the multi-tab right-click menu. Composition depends on
    /// the selection: "Create group from tabs" is always there; "Remove from
    /// group" only when the selection has an unambiguous group; "Move to
    /// group" only when there's a destination group worth offering.
    fn tab_selection_menu_items(&self) -> Vec<MenuItem<WorkspaceAction>> {
        let shared_group = self.selection_shared_group();
        let mut menu_items = vec![
            MenuItemFields::new("Create group from tabs")
                .with_on_select_action(WorkspaceAction::NewTabGroupFromSelectedTabs)
                .into_item(),
        ];

        // Only single-group selections have an unambiguous group to leave.
        if shared_group.is_some() {
            menu_items.push(
                MenuItemFields::new("Remove from group")
                    .with_on_select_action(WorkspaceAction::RemoveSelectedTabsFromGroup)
                    .into_item(),
            );
        }

        // Offer "Move to group" only when another group is available.
        let has_destination_group = self
            .tab_groups
            .keys()
            .any(|group_id| Some(*group_id) != shared_group);
        if has_destination_group {
            menu_items.push(MenuItemFields::new_submenu(MOVE_TO_GROUP_LABEL).into_item());
        }
        menu_items
    }

    /// Opens (or closes) the multi-tab right-click menu. Reuses the shared
    /// `tab_right_click_menu` view — the menu rendering pipeline doesn't need
    /// to know which item set is loaded, only which `show_*` flag is set.
    pub fn toggle_tab_selection_right_click_menu(
        &mut self,
        tab_index: usize,
        anchor: TabContextMenuAnchor,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.show_tab_selection_right_click_menu.is_some() {
            self.show_tab_selection_right_click_menu = None;
            self.hide_move_to_group_sidecar(ctx);
            ctx.notify();
            return;
        }

        let menu_items = self.tab_selection_menu_items();
        ctx.update_view(&self.tab_right_click_menu, |context_menu, view_ctx| {
            context_menu.set_items(menu_items, view_ctx);
        });
        self.show_tab_right_click_menu = None;
        self.show_tab_group_right_click_menu = None;
        self.hide_move_to_group_sidecar(ctx);
        self.show_tab_selection_right_click_menu = Some((tab_index, anchor));
        ctx.focus(&self.tab_right_click_menu);
        ctx.notify();
    }

    /// True when `tab` is positioned in the pinned region of the tab list —
    /// either because its own `pinned` flag is set (ungrouped pinned tab) or
    /// because it belongs to a pinned group.
    pub(super) fn is_tab_effectively_pinned(&self, tab: &TabData) -> bool {
        // Safety net, ensures no behavioral changes if feature flag
        // is off and some tabs have a pinned state saved.
        if !FeatureFlag::PinnedTabs.is_enabled() {
            return false;
        }
        tab.pinned
            || tab
                .group_id
                .is_some_and(|gid| self.tab_groups.get(&gid).is_some_and(|g| g.pinned))
    }

    /// Index where the unpinned region begins within `tabs`: the count of
    /// leading tabs that belong to the pinned region.
    pub(super) fn pinned_boundary_index(&self, tabs: &[TabData]) -> usize {
        tabs.iter()
            .take_while(|tab| self.is_tab_effectively_pinned(tab))
            .count()
    }

    /// Pushes `idx` past the leading effectively-pinned tabs in `tabs` if it
    /// falls inside that prefix.
    pub(super) fn clamp_to_unpinned_region(&self, tabs: &[TabData], idx: usize) -> usize {
        idx.max(self.pinned_boundary_index(tabs))
    }

    /// Returns the slot just past the last member of `group_id`, suitable as
    /// an insert/move target that keeps the group contiguous. `None` when the
    /// group has no members.
    pub(super) fn index_after_group(&self, group_id: TabGroupId) -> Option<usize> {
        group_member_indices(&self.tabs, group_id)
            .last()
            .map(|last| last + 1)
    }

    /// Pins the tab. Grouped tabs are extracted from their group first
    /// regardless of whether that group itself is pinned — tab pinning and
    /// group pinning are independent concepts.
    pub(super) fn pin_tab(&mut self, tab_index: usize, ctx: &mut ViewContext<Self>) {
        if !FeatureFlag::PinnedTabs.is_enabled() {
            return;
        }
        let Some(tab) = self.tabs.get(tab_index) else {
            log::debug!("pin_tab: tab_index {tab_index} out of bounds");
            return;
        };
        if tab.pinned {
            log::debug!("pin_tab: tab {tab_index} is already pinned");
            return;
        }
        let previous_group_id = tab.group_id;

        // Identify where this newly pinned tab should land (after the last pinned item).
        let target = self.pinned_boundary_index(&self.tabs);

        self.tabs[tab_index].group_id = None;
        self.tabs[tab_index].pinned = true;
        self.move_tab_to_index(tab_index, target, ctx);

        if let Some(prev) = previous_group_id {
            self.prune_empty_tab_group(prev, ctx);
        }

        ctx.dispatch_global_action("workspace:save_app", ());
        ctx.notify();
    }

    /// Unpins a pinned tab and moves it to the start of the unpinned region.
    pub(super) fn unpin_tab(&mut self, tab_index: usize, ctx: &mut ViewContext<Self>) {
        if !FeatureFlag::PinnedTabs.is_enabled() {
            return;
        }
        let Some(tab) = self.tabs.get(tab_index) else {
            log::debug!("unpin_tab: tab_index {tab_index} out of bounds");
            return;
        };
        if !tab.pinned {
            log::debug!("unpin_tab: tab {tab_index} is not pinned");
            return;
        }

        // This tab should land right after all pinned items.
        let target = self.pinned_boundary_index(&self.tabs);

        self.tabs[tab_index].pinned = false;
        self.move_tab_to_index(tab_index, target, ctx);

        ctx.dispatch_global_action("workspace:save_app", ());
        ctx.notify();
    }

    /// Pins the entire tab group: flips the group's `pinned` flag and moves
    /// its contiguous block of members to the end of the pinned region. We
    /// don't touch individual member `tab.pinned` flags because the block
    /// always travels as a unit, and we want to support pinning a tab even if
    /// it already belongs to a (pinned) group.
    pub(super) fn pin_tab_group(&mut self, group_id: TabGroupId, ctx: &mut ViewContext<Self>) {
        if !FeatureFlag::PinnedTabs.is_enabled() {
            return;
        }
        let Some(group) = self.tab_groups.get(&group_id) else {
            log::debug!("pin_tab_group: unknown group {group_id:?}");
            return;
        };
        if group.pinned {
            log::debug!("pin_tab_group: group {group_id:?} is already pinned");
            return;
        }

        let target = self.pinned_boundary_index(&self.tabs);
        if let Some(group) = self.tab_groups.get_mut(&group_id) {
            group.pinned = true;
        }
        self.move_group_block(group_id, target, ctx);

        ctx.dispatch_global_action("workspace:save_app", ());
        ctx.notify();
    }

    /// Unpins the entire tab group: clears the group's `pinned` flag and
    /// moves the group's block to the start of the unpinned region.
    pub(super) fn unpin_tab_group(&mut self, group_id: TabGroupId, ctx: &mut ViewContext<Self>) {
        if !FeatureFlag::PinnedTabs.is_enabled() {
            return;
        }
        let Some(group) = self.tab_groups.get(&group_id) else {
            log::debug!("unpin_tab_group: unknown group {group_id:?}");
            return;
        };
        if !group.pinned {
            log::debug!("unpin_tab_group: group {group_id:?} is not pinned");
            return;
        }

        let target = self.pinned_boundary_index(&self.tabs);
        if let Some(group) = self.tab_groups.get_mut(&group_id) {
            group.pinned = false;
        }
        self.move_group_block(group_id, target, ctx);

        ctx.dispatch_global_action("workspace:save_app", ());
        ctx.notify();
    }

    /// The framed sidebar's "Move to group" submenu: every group in sidebar order plus "Other"
    /// (no group), the current one checked and disabled, then "New group…".
    pub(super) fn build_sidebar_move_to_group_items(
        &self,
        tab_index: usize,
    ) -> Vec<MenuItem<WorkspaceAction>> {
        let current = self.tabs.get(tab_index).and_then(|tab| tab.group_id);
        let mut groups: Vec<(TabGroupId, usize)> = self
            .tab_groups
            .keys()
            .copied()
            .filter_map(|gid| {
                group_member_indices(&self.tabs, gid)
                    .next()
                    .map(|idx| (gid, idx))
            })
            .collect();
        groups.sort_by_key(|(_, idx)| *idx);
        let entry = |label: String, is_current: bool, action: WorkspaceAction| {
            let fields = MenuItemFields::new(label)
                .with_checked(is_current)
                .with_disabled(is_current);
            let fields = if is_current {
                fields
            } else {
                fields.with_indent().with_on_select_action(action)
            };
            fields.into_item()
        };
        let mut items: Vec<MenuItem<WorkspaceAction>> = groups
            .into_iter()
            .map(|(group_id, _)| {
                let name = self
                    .tab_groups
                    .get(&group_id)
                    .and_then(|g| g.name.clone())
                    .unwrap_or_else(|| "Untitled group".to_string());
                entry(
                    super::vertical_tabs::sidebar::display_group_name(&name),
                    current == Some(group_id),
                    WorkspaceAction::MoveTabToGroup {
                        tab_index,
                        group_id,
                    },
                )
            })
            .collect();
        items.push(entry(
            "Other".to_owned(),
            current.is_none(),
            WorkspaceAction::RemoveTabFromGroup(tab_index),
        ));
        items.push(MenuItem::Separator);
        items.push(
            MenuItemFields::new("New group…")
                .with_indent()
                .with_on_select_action(WorkspaceAction::NewTabGroupFromTab(tab_index))
                .into_item(),
        );
        items
    }

    /// Builds the "Move to group" submenu. One builder serves both parent
    /// menus: `Some(tab_index)` for the single-tab pane menu, `None` for the
    /// multi-tab selection menu. Destination groups exclude the source's own
    /// group (no useful move) and follow panel order so the submenu visually
    /// matches what the user sees in the tabs sidebar.
    pub(super) fn build_move_to_group_sidecar_items(
        &self,
        tab_index: Option<usize>,
    ) -> Vec<MenuItem<WorkspaceAction>> {
        // Exclude the source's current group (if any) — there's nowhere to
        // move it to. For a mixed selection (no shared group) every
        // destination stays available.
        let excluded_group = match tab_index {
            Some(idx) => self.tabs.get(idx).and_then(|tab| tab.group_id),
            None => self.selection_shared_group(),
        };

        let mut groups_with_first_index: Vec<(TabGroupId, usize)> = self
            .tab_groups
            .keys()
            .copied()
            .filter(|gid| Some(*gid) != excluded_group)
            .filter_map(|gid| {
                group_member_indices(&self.tabs, gid)
                    .next()
                    .map(|idx| (gid, idx))
            })
            .collect();
        groups_with_first_index.sort_by_key(|(_, idx)| *idx);

        groups_with_first_index
            .into_iter()
            .map(|(group_id, _)| {
                let label = self
                    .tab_groups
                    .get(&group_id)
                    .and_then(|g| g.name.clone())
                    .unwrap_or_else(|| "Untitled group".to_string());
                let action = match tab_index {
                    Some(tab_index) => WorkspaceAction::MoveTabToGroup {
                        tab_index,
                        group_id,
                    },
                    None => WorkspaceAction::MoveSelectedTabsToGroup { group_id },
                };
                MenuItemFields::new(label)
                    .with_on_select_action(action)
                    .into_item()
            })
            .collect()
    }
}

/// How often the framed sidebar asks tmux which tabs host an agent.
#[cfg(not(target_family = "wasm"))]
const TMUX_AGENT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
/// How long after the app writes a tab flag or group to tmux a read may still show the old value.
#[cfg(not(target_family = "wasm"))]
const TMUX_FLAG_WRITE_SETTLE: std::time::Duration = std::time::Duration::from_secs(2);

/// A tab's group as last agreed with its tmux session (@warp_group).
pub(super) struct TmuxTabGroup {
    name: String,
    written_at: Option<std::time::Instant>,
}

/// Whether a tmux read that started at `read_at` can see the app's last write: one that started
/// before (or right after) it may still show the old value.
#[cfg(not(target_family = "wasm"))]
fn read_sees_write(written_at: Option<std::time::Instant>, read_at: std::time::Instant) -> bool {
    written_at.is_none_or(|written| read_at > written + TMUX_FLAG_WRITE_SETTLE)
}

/// The sidebar order as last agreed with tmux (`tmux_layout`): the rev the app saw or wrote,
/// and the order that rev stands for.
#[derive(Default)]
pub(super) struct TmuxLayoutState {
    rev: Option<u64>,
    agreed: Option<LayoutSnapshot>,
    written_at: Option<std::time::Instant>,
    /// A write is on its way; no read is trusted and no second write starts until it returns.
    writing: bool,
}

/// The app's order as tmux stores it: group names, and per tmux tab its session, section and
/// position within that section.
#[derive(Clone, PartialEq)]
struct LayoutSnapshot {
    groups: Vec<String>,
    tabs: Vec<(String, Option<String>, u32)>,
}

/// A tab's inactive and marked-unread flags as last agreed with its tmux session.
pub(super) struct TmuxTabFlags {
    inactive: bool,
    unread: bool,
    written_at: Option<std::time::Instant>,
}

/// A tab's pin as last read from its tmux session (@warp_pin, contract in `sidebar_pins`), and
/// when the app last wrote it there.
pub(super) struct TmuxTabPin {
    session_id: String,
    pinned_at: Option<u64>,
    written_at: Option<std::time::Instant>,
}

impl Workspace {
    /// When the tab was pinned to the framed sidebar's "Pinned" section, if it is. A tab with a
    /// tmux session keeps its pin there; any other tab uses Warp's own tab pin, which has no time
    /// and so counts as 0.
    pub(super) fn sidebar_pin(&self, tab: &TabData) -> Option<u64> {
        match self.tmux_tab_pins.get(&tab.pane_group.id()) {
            Some(pin) => pin.pinned_at,
            None => (FeatureFlag::PinnedTabs.is_enabled() && tab.pinned).then_some(0),
        }
    }

    /// The tabs in the "Pinned" section, in its order.
    pub(super) fn sidebar_pinned_tabs(&self) -> Vec<usize> {
        sidebar_pins::order(self.tabs.iter().enumerate().filter_map(|(index, tab)| {
            let session_name = self
                .tmux_tab_session_names
                .get(&tab.pane_group.id())
                .map_or("", String::as_str);
            Some((index, self.sidebar_pin(tab)?, session_name))
        }))
    }
}

/// The framed sidebar's "Pinned" section, kept on the tmux session so every client (the phone
/// included) shows the same pins:
/// - `@warp_pin` (session option): unset = not pinned; set = the Unix second it was pinned, as a
///   decimal string. Any non-empty value pins; one that is not a number counts as 0.
/// - Pinning leaves `@warp_group` and `@warp_pos` alone, so an unpinned tab is back at its old
///   place in its group.
/// - Order: last pinned first; equal times by session name.
pub(in crate::workspace::view) mod sidebar_pins {
    /// The pin a session's `@warp_pin` value stands for.
    pub fn parse(value: &str) -> Option<u64> {
        let value = value.trim();
        (!value.is_empty()).then(|| value.parse().unwrap_or(0))
    }

    /// Tab indices in the section's order, from (index, pinned at, session name).
    pub fn order<'a>(pins: impl IntoIterator<Item = (usize, u64, &'a str)>) -> Vec<usize> {
        let mut pins: Vec<_> = pins.into_iter().collect();
        pins.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.cmp(b.2)));
        pins.into_iter().map(|(index, _, _)| index).collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn any_value_pins_and_only_unset_does_not() {
            assert_eq!(parse(""), None);
            assert_eq!(parse("1761134400"), Some(1761134400));
            assert_eq!(parse(" 1761134400\n"), Some(1761134400));
            assert_eq!(parse("yes"), Some(0));
            assert_eq!(parse("-3"), Some(0));
        }

        #[test]
        fn the_last_pinned_comes_first_and_ties_go_by_name() {
            let pins = [(0, 100, "b"), (1, 300, "z"), (2, 100, "a"), (3, 0, "c")];
            assert_eq!(order(pins), vec![1, 2, 0, 3]);
        }
    }
}

/// Whether the optional tmux integration is on (`integrations.tmux.enabled`, off by default).
/// Read on every use, so turning it on or off needs no restart.
#[cfg(not(target_family = "wasm"))]
fn tmux_sync_enabled(ctx: &warpui::AppContext) -> bool {
    *TabSettings::as_ref(ctx).tmux_sync_enabled.value()
}

/// The script set in `integrations.tmux.session_script`, if it exists. Without one, the app
/// renames and regroups sessions with plain tmux commands.
#[cfg(not(target_family = "wasm"))]
fn tmux_session_script(ctx: &warpui::AppContext) -> Option<std::path::PathBuf> {
    let script = TabSettings::as_ref(ctx).tmux_session_script.value().trim().to_owned();
    if script.is_empty() {
        return None;
    }
    let path = std::path::PathBuf::from(shellexpand::tilde(&script).into_owned());
    if path.exists() {
        Some(path)
    } else {
        log::warn!("tmux session script {} does not exist; using tmux commands", path.display());
        None
    }
}

#[cfg(not(target_family = "wasm"))]
impl Workspace {
    /// Polls tmux while the framed sidebar and the tmux integration are on, so tabs attached to a
    /// tmux session that runs an agent count as agent tabs before that agent sends its first event
    /// to this window, and tabs follow their sessions.
    pub(super) fn schedule_tmux_agent_poll(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.spawn(
            warpui::r#async::Timer::after(TMUX_AGENT_POLL_INTERVAL),
            |me, _, ctx| {
                me.poll_tmux_agents(ctx);
                if me.uses_framed_sidebar_layout(ctx) && tmux_sync_enabled(ctx) {
                    me.poll_tmux_tab_sync(ctx);
                    me.poll_tmux_adopt(ctx);
                }
                me.schedule_tmux_agent_poll(ctx);
            },
        );
    }

    fn poll_tmux_agents(&mut self, ctx: &mut ViewContext<Self>) {
        if !self.uses_framed_sidebar_layout(ctx) {
            return;
        }
        if !tmux_sync_enabled(ctx) {
            // Pins live on tmux sessions only; without the integration there is no "Pinned".
            if !self.tmux_agent_terminals.is_empty() || !self.tmux_tab_pins.is_empty() {
                self.tmux_agent_terminals.clear();
                self.tmux_tab_pins.clear();
                ctx.notify();
            }
            return;
        }
        let shells: Vec<(EntityId, u32)> = self
            .tabs
            .iter()
            .flat_map(|tab| {
                super::vertical_tabs::tab_terminal_shell_pids(tab.pane_group.as_ref(ctx), ctx)
            })
            .collect();
        if shells.is_empty() && self.tmux_agent_terminals.is_empty() {
            return;
        }
        ctx.spawn(
            async move { tokio::task::spawn_blocking(tmux_agents::tmux_agent_shell_pids).await },
            move |me, agent_shells, ctx| {
                let agent_shells = agent_shells.unwrap_or_default();
                let found: HashSet<EntityId> = shells
                    .iter()
                    .filter(|(_, pid)| agent_shells.contains(pid))
                    .map(|(terminal_view_id, _)| *terminal_view_id)
                    .collect();
                if found != me.tmux_agent_terminals {
                    me.tmux_agent_terminals = found;
                    ctx.notify();
                }
            },
        );
    }
}

#[cfg(not(target_family = "wasm"))]
impl Workspace {
    /// With the tmux integration on, a tab renamed by hand renames the tmux session it is
    /// attached to. The session script, when set, gets `rename <shell_pid> <group> <title>` and
    /// may name the session as it likes (e.g. with the group in front); otherwise the app runs
    /// `tmux rename-session` with the title. Either way it runs detached, off the UI thread.
    pub(super) fn rename_tmux_sessions_of_tab(
        &mut self,
        tab_index: usize,
        title: &str,
        ctx: &mut ViewContext<Self>,
    ) {
        if !tmux_sync_enabled(ctx) {
            return;
        }
        let Some(tab) = self.tabs.get(tab_index) else {
            return;
        };
        let group = tab
            .group_id
            .and_then(|group_id| self.tab_groups.get(&group_id))
            .and_then(|group| group.name.clone())
            .unwrap_or_default();
        let pane_group_id = tab.pane_group.id();
        // A split tab follows its first tmux terminal, the same one the sync follows; the other
        // panes' sessions keep their own names.
        let Some((_, shell_pid)) =
            super::vertical_tabs::tab_terminal_shell_pids(tab.pane_group.as_ref(ctx), ctx)
                .into_iter()
                .next()
        else {
            return;
        };
        // Until the session carries the new name, the sync would put the old one back.
        self.tmux_rename_settle.insert(
            pane_group_id,
            TmuxRenameSettle {
                old_session_name: self.tmux_tab_session_names.get(&pane_group_id).cloned(),
                started: std::time::Instant::now(),
                warned: false,
            },
        );
        tmux_agents::rename_session(tmux_session_script(ctx), shell_pid, &group, title);
    }

    /// Keeps every tmux tab in step with its session: the tab shows the session's name (so a
    /// name set by another tool or by hand in tmux shows up in the sidebar), its group is stored
    /// on the session (@warp_group), and a tab seen for the first time takes the group its
    /// session remembers.
    fn poll_tmux_tab_sync(&mut self, ctx: &mut ViewContext<Self>) {
        let shells: Vec<(EntityId, u32)> = self
            .tabs
            .iter()
            .flat_map(|tab| {
                let pane_group_id = tab.pane_group.id();
                super::vertical_tabs::tab_terminal_shell_pids(tab.pane_group.as_ref(ctx), ctx)
                    .into_iter()
                    .map(move |(_, pid)| (pane_group_id, pid))
            })
            .collect();
        if shells.is_empty() {
            return;
        }
        let read_at = std::time::Instant::now();
        ctx.spawn(
            async move {
                tokio::task::spawn_blocking(|| {
                    (
                        tmux_agents::tmux_client_sessions(),
                        tmux_agents::read_layout_globals(),
                    )
                })
                .await
            },
            move |me, read, ctx| {
                let (sessions, layout) = read.unwrap_or_default();
                me.apply_tmux_tab_sync(&shells, &sessions, layout, read_at, ctx);
            },
        );
    }

    /// Syncs with tmux right away instead of at the next poll, so a toggle reaches other tmux
    /// clients now.
    pub(in crate::workspace::view) fn sync_tmux_tabs_now(&mut self, ctx: &mut ViewContext<Self>) {
        if self.uses_framed_sidebar_layout(ctx) && tmux_sync_enabled(ctx) {
            self.poll_tmux_tab_sync(ctx);
        }
    }

    fn apply_tmux_tab_sync(
        &mut self,
        shells: &[(EntityId, u32)],
        sessions: &std::collections::HashMap<u32, tmux_agents::ClientSession>,
        layout: Option<(Option<u64>, Vec<String>)>,
        read_at: std::time::Instant,
        ctx: &mut ViewContext<Self>,
    ) {
        let open_tabs: HashSet<EntityId> = self.tabs.iter().map(|tab| tab.pane_group.id()).collect();
        let mut tab_sessions: std::collections::HashMap<EntityId, tmux_agents::ClientSession> =
            std::collections::HashMap::new();
        self.tmux_tab_groups.retain(|id, _| open_tabs.contains(id));
        self.tmux_tab_session_names.retain(|id, _| open_tabs.contains(id));
        self.tmux_tab_flags.retain(|id, _| open_tabs.contains(id));
        self.tmux_tab_pins.retain(|id, _| open_tabs.contains(id));
        let mut flags_changed = false;
        let paths_before = std::mem::take(&mut self.tmux_tab_paths);
        self.tmux_rename_settle.retain(|id, _| open_tabs.contains(id));
        let mut regrouped = false;
        let mut synced = HashSet::new();
        for &(pane_group_id, pid) in shells {
            let Some(session) = sessions.get(&pid) else {
                continue;
            };
            // One session per tab is enough; a split tab follows its first tmux terminal.
            if !synced.insert(pane_group_id) {
                continue;
            }
            let Some(index) = self
                .tabs
                .iter()
                .position(|tab| tab.pane_group.id() == pane_group_id)
            else {
                continue;
            };
            tab_sessions.insert(pane_group_id, session.clone());

            self.tmux_tab_session_names
                .insert(pane_group_id, session.name.clone());
            if !session.path.is_empty() {
                self.tmux_tab_paths
                    .insert(pane_group_id, session.path.clone());
            }
            if !self.rename_still_settling(pane_group_id, &session.name) {
                self.tabs[index].pane_group.update(ctx, |view, ctx| {
                    if view.custom_title(ctx).as_deref() != Some(session.name.as_str()) {
                        view.set_title(&session.name, ctx);
                    }
                });
            }

            flags_changed |= self.sync_tmux_tab_flags(index, session, read_at);
            flags_changed |= self.sync_tmux_tab_pin(pane_group_id, session, read_at);

            regrouped |= self.sync_tmux_tab_group(index, session, read_at, ctx);
        }
        if self.tmux_tab_paths != paths_before || flags_changed {
            ctx.notify();
        }
        if regrouped {
            self.keep_grouped_tabs_first(ctx);
            ctx.dispatch_global_action("workspace:save_app", ());
            ctx.notify();
        }
        // The order step runs after the group step, so membership is settled first.
        if let Some(layout) = layout {
            self.sync_tmux_layout(&tab_sessions, layout, read_at, ctx);
        }
    }

    /// The sidebar order as `tmux_layout` describes it, from the app's tabs; `pos` per tab from
    /// its tmux session (`None` without one).
    fn layout_tabs(
        &self,
        tab_sessions: &std::collections::HashMap<EntityId, tmux_agents::ClientSession>,
    ) -> Vec<tmux_layout::LayoutTab> {
        self.tabs
            .iter()
            .map(|tab| tmux_layout::LayoutTab {
                pinned: self.is_tab_effectively_pinned(tab),
                group: tab
                    .group_id
                    .and_then(|group_id| self.tab_groups.get(&group_id))
                    .and_then(|group| group.name.clone()),
                pos: tab_sessions
                    .get(&tab.pane_group.id())
                    .and_then(|session| session.pos),
            })
            .collect()
    }

    /// Keeps the sidebar order and tmux's `@warp_pos` / `@warp_groups` / `@warp_layout_rev` in
    /// step (contract in `tmux_layout`). A newer rev from another writer is adopted; otherwise a
    /// change of the app's own order since the last agreed state is written, positions for the
    /// tmux tabs whose position moved, the merged group list, then rev + 1, but only if tmux
    /// still holds the rev this read saw.
    fn sync_tmux_layout(
        &mut self,
        tab_sessions: &std::collections::HashMap<EntityId, tmux_agents::ClientSession>,
        (read_rev, tmux_groups): (Option<u64>, Vec<String>),
        read_at: std::time::Instant,
        ctx: &mut ViewContext<Self>,
    ) {
        // Mid-drag the sidebar shows a drop target over a still order; leave it alone.
        if self.current_workspace_state.is_tab_being_dragged || self.is_group_being_dragged {
            return;
        }
        let fresh = !self.tmux_layout.writing
            && read_sees_write(self.tmux_layout.written_at, read_at);
        if tmux_layout::should_adopt(read_rev, self.tmux_layout.rev, fresh) {
            let tabs = self.layout_tabs(tab_sessions);
            let order: Vec<EntityId> = tmux_layout::adopted_order(&tabs, &tmux_groups)
                .into_iter()
                .map(|index| self.tabs[index].pane_group.id())
                .collect();
            let mut moved = false;
            for (to_index, pane_group_id) in order.into_iter().enumerate() {
                if let Some(from_index) = self
                    .tabs
                    .iter()
                    .position(|tab| tab.pane_group.id() == pane_group_id)
                    && from_index != to_index
                {
                    self.hop_tab_to_index(from_index, to_index, ctx);
                    moved = true;
                }
            }
            if moved {
                self.keep_grouped_tabs_first(ctx);
                ctx.dispatch_global_action("workspace:save_app", ());
                ctx.notify();
            }
            self.tmux_layout.rev = read_rev;
            self.tmux_layout.agreed = Some(self.layout_snapshot(tab_sessions));
            return;
        }
        if !fresh || !tmux_layout::may_write(tab_sessions.len()) {
            return;
        }
        let snapshot = self.layout_snapshot(tab_sessions);
        if read_rev.is_some() && self.tmux_layout.agreed.as_ref() == Some(&snapshot) {
            return;
        }
        let tabs = self.layout_tabs(tab_sessions);
        let positions: Vec<(String, u32)> = self
            .tabs
            .iter()
            .zip(tmux_layout::section_positions(&tabs))
            .filter_map(|(tab, pos)| {
                let session = tab_sessions.get(&tab.pane_group.id())?;
                let pos = pos?;
                (session.pos != Some(pos)).then(|| (session.id.clone(), pos))
            })
            .collect();
        let merged = tmux_layout::merge_group_order(&tmux_groups, &snapshot.groups);
        let groups = (merged != tmux_groups).then(|| merged.join("\n"));
        if read_rev.is_some() && positions.is_empty() && groups.is_none() {
            // tmux already says what the app shows (e.g. after adopting): agree, no write.
            self.tmux_layout.agreed = Some(snapshot);
            self.tmux_layout.rev = read_rev;
            return;
        }
        self.tmux_layout.writing = true;
        self.tmux_layout.written_at = Some(std::time::Instant::now());
        ctx.spawn(
            async move {
                tokio::task::spawn_blocking(move || {
                    tmux_agents::write_layout(read_rev, &positions, groups.as_deref())
                })
                .await
            },
            move |me, written, _| {
                me.tmux_layout.writing = false;
                me.tmux_layout.written_at = Some(std::time::Instant::now());
                // Not written (another client changed the order first): the next poll adopts.
                if let Ok(Some(rev)) = written {
                    me.tmux_layout.rev = Some(rev);
                    me.tmux_layout.agreed = Some(snapshot);
                }
            },
        );
    }

    fn layout_snapshot(
        &self,
        tab_sessions: &std::collections::HashMap<EntityId, tmux_agents::ClientSession>,
    ) -> LayoutSnapshot {
        let tabs = self.layout_tabs(tab_sessions);
        let positions = tmux_layout::section_positions(&tabs);
        LayoutSnapshot {
            groups: tmux_layout::app_group_order(&tabs),
            tabs: self
                .tabs
                .iter()
                .zip(tabs)
                .zip(positions)
                .filter_map(|((tab, layout_tab), pos)| {
                    let session = tab_sessions.get(&tab.pane_group.id())?;
                    Some((session.id.clone(), layout_tab.group, pos?))
                })
                .collect(),
        }
    }

    /// Inactive and marked-unread live on the tmux session too, so other tmux clients see them and
    /// they survive a restart. Whoever changed a flag since the last agreed value wins: a change
    /// in the app is written to the session, a change in tmux (another client) is taken over. On the
    /// first match the session's values win, which restores them after a restart. Returns
    /// whether the tab changed.
    fn sync_tmux_tab_flags(
        &mut self,
        index: usize,
        session: &tmux_agents::ClientSession,
        read_at: std::time::Instant,
    ) -> bool {
        let tab = &mut self.tabs[index];
        let pane_group_id = tab.pane_group.id();
        let in_tmux = (session.inactive, session.unread);
        let Some(agreed) = self.tmux_tab_flags.get_mut(&pane_group_id) else {
            let changed = (tab.inactive, tab.marked_unread) != in_tmux;
            (tab.inactive, tab.marked_unread) = in_tmux;
            self.tmux_tab_flags.insert(
                pane_group_id,
                TmuxTabFlags { inactive: in_tmux.0, unread: in_tmux.1, written_at: None },
            );
            return changed;
        };
        let read_is_fresh = read_sees_write(agreed.written_at, read_at);
        let mut changed = false;
        let mut wrote = false;
        for (option, in_app, in_tmux, last) in [
            ("@warp_inactive", &mut tab.inactive, in_tmux.0, &mut agreed.inactive),
            ("@warp_unread", &mut tab.marked_unread, in_tmux.1, &mut agreed.unread),
        ] {
            if *in_app != *last {
                tmux_agents::set_session_flag(&session.id, option, *in_app);
                *last = *in_app;
                wrote = true;
            } else if in_tmux != *last && read_is_fresh {
                *in_app = in_tmux;
                *last = in_tmux;
                changed = true;
            }
        }
        if wrote {
            agreed.written_at = Some(std::time::Instant::now());
        }
        changed
    }

    /// The pin is only ever changed by writing it to the session (`set_tab_pinned_in_sidebar`),
    /// so the app takes what tmux says, except from a read too early to see its own last write.
    /// Returns whether the pin changed.
    fn sync_tmux_tab_pin(
        &mut self,
        pane_group_id: EntityId,
        session: &tmux_agents::ClientSession,
        read_at: std::time::Instant,
    ) -> bool {
        let pin = self
            .tmux_tab_pins
            .entry(pane_group_id)
            .or_insert(TmuxTabPin {
                session_id: session.id.clone(),
                pinned_at: None,
                written_at: None,
            });
        pin.session_id.clone_from(&session.id);
        if pin.pinned_at == session.pin || !read_sees_write(pin.written_at, read_at) {
            return false;
        }
        pin.pinned_at = session.pin;
        true
    }

    /// Pins the tab's tmux session to the framed sidebar's "Pinned" section (`@warp_pin` = now)
    /// or unpins it (`sidebar_pins`). A tab without a session has nothing to pin.
    pub(super) fn set_tab_pinned_in_sidebar(
        &mut self,
        pane_group_id: EntityId,
        pinned: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(pin) = self.tmux_tab_pins.get_mut(&pane_group_id) else {
            return;
        };
        let pinned_at = pinned.then(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_secs())
        });
        tmux_agents::set_session_option(
            &pin.session_id,
            "@warp_pin",
            pinned_at.map(|at| at.to_string()).as_deref(),
        );
        pin.pinned_at = pinned_at;
        pin.written_at = Some(std::time::Instant::now());
        ctx.notify();
    }

    /// "Pin" or "Unpin" for the vertical tabs' tab menu, first in it: in the framed sidebar a tab
    /// with a tmux session pins that session, every other tab takes Warp's own tab pin.
    pub(super) fn add_sidebar_pin_menu_item(
        &self,
        tab_index: usize,
        menu_items: &mut Vec<MenuItem<WorkspaceAction>>,
        ctx: &warpui::AppContext,
    ) {
        if !crate::tab::uses_vertical_tabs(ctx) {
            return;
        }
        let Some(tab) = self.tabs.get(tab_index) else {
            return;
        };
        let pane_group_id = tab.pane_group.id();
        let session_pin = self
            .tmux_tab_pins
            .get(&pane_group_id)
            .filter(|_| self.uses_framed_sidebar_layout(ctx));
        let (pinned, action) = match session_pin {
            Some(pin) => {
                let pinned = pin.pinned_at.is_some();
                let action = WorkspaceAction::SetTabPinnedInSidebar {
                    pane_group_id,
                    pinned: !pinned,
                };
                (pinned, action)
            }
            None if FeatureFlag::PinnedTabs.is_enabled() => {
                let action = if tab.pinned {
                    WorkspaceAction::UnpinTab(tab_index)
                } else {
                    WorkspaceAction::PinTab(tab_index)
                };
                (tab.pinned, action)
            }
            None => return,
        };
        menu_items.insert(
            0,
            MenuItemFields::new(if pinned { "Unpin" } else { "Pin" })
                .with_on_select_action(action)
                .into_item(),
        );
    }

    /// The tab's group follows the same rule as its flags: whoever changed it since the last
    /// agreed value wins, so a group set from another client holds. On the first match an ungrouped tab
    /// takes the group its session remembers, which is how groups survive a restart. Returns
    /// whether the tab moved.
    fn sync_tmux_tab_group(
        &mut self,
        index: usize,
        session: &tmux_agents::ClientSession,
        read_at: std::time::Instant,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let pane_group_id = self.tabs[index].pane_group.id();
        let previous_group_id = self.tabs[index].group_id;
        let current = previous_group_id
            .and_then(|group_id| self.tab_groups.get(&group_id))
            .and_then(|group| group.name.clone())
            .unwrap_or_default();
        let take_tmux = match self.tmux_tab_groups.get(&pane_group_id) {
            None => current.is_empty() && !session.group.is_empty(),
            Some(agreed) if agreed.name != current => false,
            Some(agreed) => {
                session.group != agreed.name && read_sees_write(agreed.written_at, read_at)
            }
        };
        if !take_tmux {
            let agreed = self.tmux_tab_groups.entry(pane_group_id).or_insert(TmuxTabGroup {
                name: session.group.clone(),
                written_at: None,
            });
            // New to the app (first match) or changed in the app: the session takes it. A push
            // that failed is not retried; the next fresh read then takes the session's group.
            if agreed.name != current {
                tmux_agents::set_session_group(
                    tmux_session_script(ctx),
                    &session.id,
                    &session.group,
                    &current,
                );
                agreed.name = current;
                agreed.written_at = Some(std::time::Instant::now());
            }
            return false;
        }
        // The tab joins its group's members, as when moved by hand, without taking focus (this
        // runs in the background). An ungrouped tab goes to the end of the "other" section.
        let existing = self
            .tab_groups
            .values()
            .find(|group| group.name.as_deref() == Some(session.group.as_str()))
            .map(|group| group.id);
        let target = match existing {
            _ if session.group.is_empty() => Some(self.tabs.len()),
            Some(group_id) => self.index_after_group(group_id),
            None => None,
        };
        if session.group.is_empty() {
            self.tabs[index].group_id = None;
        } else {
            self.put_tab_in_named_group(index, &session.group);
        }
        if let Some(target) = target {
            self.move_tab_to_index(index, target, ctx);
        }
        if let Some(previous) = previous_group_id {
            if existing != Some(previous) {
                self.prune_empty_tab_group(previous, ctx);
            }
        }
        self.tmux_tab_groups.insert(
            pane_group_id,
            TmuxTabGroup { name: session.group.clone(), written_at: None },
        );
        true
    }

    /// True while a tab renamed by hand waits for its session to take the new name. Without a known
    /// old name the wait is TMUX_RENAME_SETTLE. A rename that never lands keeps the typed title
    /// (no silent revert) and is logged once after TMUX_RENAME_WARN.
    fn rename_still_settling(&mut self, pane_group_id: EntityId, session_name: &str) -> bool {
        let Some(settle) = self.tmux_rename_settle.get_mut(&pane_group_id) else {
            return false;
        };
        let waiting = match &settle.old_session_name {
            Some(old) => old == session_name,
            None => settle.started.elapsed() < TMUX_RENAME_SETTLE,
        };
        if !waiting {
            self.tmux_rename_settle.remove(&pane_group_id);
            return false;
        }
        if !settle.warned && settle.started.elapsed() >= TMUX_RENAME_WARN {
            settle.warned = true;
            log::warn!("tmux session {session_name} did not take the tab's new name; keeping the typed title");
        }
        true
    }

    /// Puts a tab into the group with this name, creating the group if there is none yet.
    fn put_tab_in_named_group(&mut self, tab_index: usize, name: &str) {
        let group_id = self
            .tab_groups
            .values()
            .find(|group| group.name.as_deref() == Some(name))
            .map(|group| group.id)
            .unwrap_or_else(|| {
                let mut group = TabGroup::new();
                group.name = Some(name.to_owned());
                let id = group.id;
                self.tab_groups.insert(id, group);
                id
            });
        let tab = &mut self.tabs[tab_index];
        tab.group_id = Some(group_id);
        tab.pinned = false;
    }
}

/// How often Claude Code's session files are read for dialogs that fire no hook.
#[cfg(not(target_family = "wasm"))]
const CLAUDE_WAIT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(not(target_family = "wasm"))]
impl Workspace {
    /// Every 2 s while the framed sidebar and the tmux integration are on, shows "needs you" for
    /// agent tabs whose Claude Code session file says it waits on a dialog no hook reports
    /// (`claude_waits`). With the integration off, any such override is dropped.
    pub(super) fn schedule_claude_wait_poll(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.spawn(
            warpui::r#async::Timer::after(CLAUDE_WAIT_POLL_INTERVAL),
            |me, _, ctx| {
                me.sync_claude_notices(ctx);
                me.poll_claude_waits(ctx);
                me.schedule_claude_wait_poll(ctx);
            },
        );
    }

    fn poll_claude_waits(&mut self, ctx: &mut ViewContext<Self>) {
        let sessions = CLIAgentSessionsModel::as_ref(ctx);
        let terminals: Vec<(EntityId, u32)> = self
            .tabs
            .iter()
            .flat_map(|tab| {
                super::vertical_tabs::tab_terminal_shell_pids(tab.pane_group.as_ref(ctx), ctx)
            })
            .filter(|(terminal_view_id, _)| sessions.session(*terminal_view_id).is_some())
            .collect();
        if terminals.is_empty() {
            return;
        }
        if !self.uses_framed_sidebar_layout(ctx) || !tmux_sync_enabled(ctx) {
            Self::apply_claude_waits(&terminals, &Default::default(), ctx);
            return;
        }
        let shells = terminals.clone();
        ctx.spawn(
            async move {
                tokio::task::spawn_blocking(move || tmux_agents::claude_waits_by_terminal(&shells))
                    .await
            },
            move |_, waits, ctx| {
                let Ok(waits) = waits else {
                    return;
                };
                Self::apply_claude_waits(&terminals, &waits, ctx);
            },
        );
    }

    fn apply_claude_waits(
        terminals: &[(EntityId, u32)],
        waits: &std::collections::HashMap<EntityId, claude_waits::Wait>,
        ctx: &mut ViewContext<Self>,
    ) {
        CLIAgentSessionsModel::handle(ctx).update(ctx, |model, ctx| {
            for (terminal_view_id, _) in terminals {
                match waits.get(terminal_view_id) {
                    Some(wait) => model.set_external_wait(
                        *terminal_view_id,
                        Some(wait.label.clone()),
                        wait.updated_ms,
                        ctx,
                    ),
                    None => model.set_external_wait(*terminal_view_id, None, 0, ctx),
                }
            }
        });
    }
}

/// One window's Claude dialog notifications (`claude_waits::notify`).
#[cfg(not(target_family = "wasm"))]
#[derive(Default)]
pub(super) struct ClaudeNotices {
    entries: std::collections::HashMap<EntityId, claude_waits::notify::Entry<NoticeTarget>>,
    /// Terminals an Allow or Deny is being typed into: one answer at a time each.
    answering: HashSet<EntityId>,
    /// Terminals whose answer from a notification closed their dialog since the last sync.
    answered: HashSet<EntityId>,
}

/// Where a Claude dialog's notification points.
#[cfg(not(target_family = "wasm"))]
#[derive(Clone, Debug)]
pub(super) struct NoticeTarget {
    pane_group_id: EntityId,
    pane_id: crate::pane_group::PaneId,
    /// The session's name, the notification's title.
    title: String,
    /// The dialog was read from the normal screen, where the pty's keys reach Claude itself.
    answerable: bool,
}

/// Notification identifiers, unique across the app's windows.
#[cfg(not(target_family = "wasm"))]
static NOTICE_IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Notification permission is asked once per run, before the first Claude notification: a
/// mode set in settings.toml never went through the settings page that asks.
#[cfg(not(target_family = "wasm"))]
static NOTICE_PERMISSION_ASKED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(not(target_family = "wasm"))]
fn next_notice_id() -> String {
    let n = NOTICE_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("harp-claude-{}-{n}", std::process::id())
}

/// The visible rows of a terminal's screen, from Warp's own grid, and whether keys written to
/// its pty reach what it shows: not through a multiplexer, which can draw one pane and send
/// them to another. Claude Code 2.1 draws on the alternate screen, so that one is read too.
#[cfg(not(target_family = "wasm"))]
fn claude_screen(view: &crate::terminal::TerminalView) -> (String, bool) {
    let model = view.model.lock();
    let block = model.block_list().active_block();
    if model.is_alt_screen_active() {
        let answerable = !claude_waits::dialog::runs_multiplexer(&block.command_to_string());
        return (model.alt_screen().output_to_string(), ANSWER_FROM_NOTIFICATIONS && answerable);
    }
    let rows = model.block_list().size().rows();
    (block.output_grid().contents_to_string(false, Some(rows)), ANSWER_FROM_NOTIFICATIONS)
}

/// ponytail: Allow and Deny type keys into the pty, which can race the user's own keys, reach
/// another pane of a multiplexer started through a wrapper, or answer a repeat of the same dialog
/// (review, 26 Oct). Until an answer is bound to one permission request, notifications only open
/// the chat.
#[cfg(not(target_family = "wasm"))]
const ANSWER_FROM_NOTIFICATIONS: bool = false;

/// Whether the sidebar shows this terminal's Claude Code as "needs you".
#[cfg(not(target_family = "wasm"))]
fn claude_needs_you(view: &crate::terminal::TerminalView, ctx: &warpui::AppContext) -> bool {
    use crate::ai::agent::conversation::ConversationStatus;
    let claude = CLIAgentSessionsModel::as_ref(ctx)
        .session(view.id())
        .is_some_and(|session| matches!(session.agent, crate::terminal::CLIAgent::Claude));
    claude
        && matches!(
            super::vertical_tabs::summary_conversation_status_for_terminal(view, ctx),
            Some(ConversationStatus::Blocked { .. })
        )
}

#[cfg(not(target_family = "wasm"))]
impl Workspace {
    /// With the Claude wait poll (2 s): reads the screen of every shown terminal whose Claude
    /// Code needs the user, sends one notification per new dialog unless its terminal is in
    /// view, and takes back those answered elsewhere or whose terminal closed. Follows the
    /// needs-attention notification setting, whose plain notification it replaces for Claude.
    fn sync_claude_notices(&mut self, ctx: &mut ViewContext<Self>) {
        use crate::terminal::session_settings::{NotificationsMode, SessionSettings};
        use claude_waits::notify::{self, Change};
        let settings = SessionSettings::as_ref(ctx).notifications.value();
        let enabled =
            settings.mode == NotificationsMode::Enabled && settings.is_needs_attention_enabled;
        if !enabled && self.claude_notices.entries.is_empty() {
            return;
        }
        let waiting = if enabled {
            self.claude_waiting_screens(ctx)
        } else {
            Vec::new()
        };
        let in_view: HashSet<EntityId> = notify::in_view(
            ctx.windows().app_is_active(),
            ctx.windows().active_window() == Some(ctx.window_id()),
            self.tabs.get(self.active_tab_index),
        )
        .map(|tab| super::vertical_tabs::tab_terminal_view_ids(tab.pane_group.as_ref(ctx), ctx))
        .unwrap_or_default()
        .into_iter()
        .collect();
        // An answer still being typed counts too: a sync can fall between its Enter and its end.
        let answered: HashSet<EntityId> = std::mem::take(&mut self.claude_notices.answered)
            .into_iter()
            .chain(self.claude_notices.answering.iter().copied())
            .collect();
        let changes = notify::update(
            &mut self.claude_notices.entries,
            waiting,
            &in_view,
            &answered,
            next_notice_id,
        );
        for change in changes {
            match change {
                Change::TakeBack(id) => ctx.remove_desktop_notification(&id),
                Change::Post(terminal) => {
                    let Some(entry) = self.claude_notices.entries.get(&terminal) else {
                        continue;
                    };
                    let (category, body) = notify::content(&entry.card, entry.target.answerable);
                    let (id, target) = (entry.id.clone(), entry.target.clone());
                    log::info!("Claude dialog notification {id}: {category}");
                    self.send_claude_notice(terminal, &target, id, category, body, ctx);
                }
            }
        }
    }

    /// Each shown terminal whose Claude Code needs the user, with the dialog its screen shows,
    /// or the wait's own message when no dialog can be read.
    fn claude_waiting_screens(
        &self,
        ctx: &ViewContext<Self>,
    ) -> Vec<(EntityId, claude_waits::notify::Read<NoticeTarget>)> {
        use crate::terminal::cli_agent_sessions::CLIAgentSessionStatus;
        use claude_waits::dialog::{self, Card};
        let sessions = CLIAgentSessionsModel::as_ref(ctx);
        let mut waiting = Vec::new();
        for tab in &self.tabs {
            let pane_group = tab.pane_group.as_ref(ctx);
            for (pane_id, view) in super::vertical_tabs::tab_terminal_views(pane_group, ctx) {
                let view = view.as_ref(ctx);
                if !claude_needs_you(view, ctx) {
                    continue;
                }
                let session = sessions.session(view.id());
                let message = session
                    .and_then(|session| match &session.status {
                        CLIAgentSessionStatus::Blocked { message, .. } => message.clone(),
                        _ => None,
                    })
                    .unwrap_or_else(|| "Claude needs you".to_owned());
                let title = session
                    .and_then(|session| session.session_context.display_title())
                    .unwrap_or_else(|| "Claude Code".to_owned());
                let (screen, answerable) = claude_screen(view);
                let card = dialog::card_on_screen(&screen).unwrap_or(Card::Waiting(message));
                let target = NoticeTarget {
                    pane_group_id: tab.pane_group.id(),
                    pane_id,
                    title,
                    answerable,
                };
                waiting.push((view.id(), claude_waits::notify::Read { card, target }));
            }
        }
        waiting
    }

    /// Sends one Claude notification; its data names the window, the pane and its own id.
    fn send_claude_notice(
        &self,
        terminal: EntityId,
        target: &NoticeTarget,
        id: String,
        category: &str,
        body: String,
        ctx: &mut ViewContext<Self>,
    ) {
        use crate::terminal::session_settings::SessionSettings;
        use warpui::notification::UserNotification;
        let data = crate::notification::NotificationContext::ClaudeWait {
            window_id: ctx.window_id(),
            pane_group_id: target.pane_group_id,
            pane_id: target.pane_id,
            terminal_view_id: terminal,
            id: id.clone(),
        };
        let Ok(data) = serde_json::to_string(&data) else {
            return;
        };
        let play_sound = SessionSettings::as_ref(ctx)
            .notifications
            .play_notification_sound;
        let content =
            UserNotification::new_with_sound(target.title.clone(), body, Some(data), play_sound)
                .with_identifier(id)
                .with_category(category);
        let failed = |_: &mut Self,
                      error: warpui::notification::NotificationSendError,
                      _: &mut ViewContext<Self>| {
            log::warn!("Claude notification not sent: {error:?}");
        };
        if NOTICE_PERMISSION_ASKED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            ctx.send_desktop_notification(content, failed);
        } else {
            // macOS asks once (nothing when already decided), then the first one goes out.
            ctx.request_desktop_notification_permissions(move |_, _, ctx| {
                ctx.send_desktop_notification(content, failed);
            });
        }
    }

    /// A click on a Claude notification (lib.rs `on_notification_clicked`). Allow or Deny
    /// types the answer into the terminal that showed the dialog, re-reading its screen before
    /// every key; a dialog that is gone or changed gets no key and a notification saying so.
    /// Returns whether the pane should be focused (Open chat, or a click on the notification).
    pub(crate) fn claude_notice_clicked(
        &mut self,
        click: &crate::notification::ClaudeNoticeClick,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        use claude_waits::answer::Answer;
        use claude_waits::dialog::Card;
        use claude_waits::notify::{self, Act};
        let terminal = click.terminal_view_id;
        ctx.remove_desktop_notification(&click.id);
        let entry = notify::clicked(&mut self.claude_notices.entries, &terminal, &click.id)
            .map(|entry| (entry.card.clone(), entry.target.clone()));
        let answerable = entry.as_ref().is_some_and(|(_, target)| target.answerable);
        let card = entry.as_ref().map(|(card, _)| card);
        match notify::act(click.action.as_deref(), card, answerable) {
            Act::Open => true,
            Act::Refuse(reason) => {
                let target = entry.map(|(_, target)| target).unwrap_or_else(|| NoticeTarget {
                    pane_group_id: click.pane_group_id,
                    pane_id: click.pane_id,
                    title: "Claude Code".to_owned(),
                    answerable: false,
                });
                self.claude_notice_refused(terminal, &target, reason, ctx);
                false
            }
            Act::Pick(index) => {
                let Some((Card::Approval(dialog), target)) = entry else {
                    return false;
                };
                if self.claude_notices.answering.insert(terminal) {
                    self.step_claude_answer(terminal, target, Answer::new(dialog, index), ctx);
                }
                false
            }
        }
    }

    /// One step of an Allow or Deny: reads the terminal, presses the next key through its
    /// pty, and comes back after `KEY_STEP` until the dialog closed or the answer refused.
    fn step_claude_answer(
        &mut self,
        terminal: EntityId,
        target: NoticeTarget,
        mut answer: claude_waits::answer::Answer,
        ctx: &mut ViewContext<Self>,
    ) {
        use crate::terminal::model::escape_sequences::EscCodes;
        use claude_waits::answer::{KEY_STEP, Key, Step, TERMINAL_GONE};
        let view = self.claude_terminal(terminal, &target, ctx);
        let step = match &view {
            None => Step::Refuse(TERMINAL_GONE),
            Some(view) => {
                let view = view.as_ref(ctx);
                let (screen, answerable) = claude_screen(view);
                let now = claude_waits::dialog::dialog_on_screen(&screen);
                answer.next(now.as_ref(), answerable && claude_needs_you(view, ctx))
            }
        };
        let press = match step {
            Step::Press(key) => Some(key),
            Step::Wait => None,
            Step::Done => {
                log::info!("Claude dialog answered from its notification");
                self.claude_notices.answering.remove(&terminal);
                self.claude_notices.answered.insert(terminal);
                return;
            }
            Step::Refuse(reason) => {
                log::info!("Claude dialog answer refused: {reason}");
                self.claude_notices.answering.remove(&terminal);
                self.claude_notice_refused(terminal, &target, reason, ctx);
                return;
            }
        };
        if let (Some(key), Some(view)) = (press, &view) {
            let bytes = {
                let model = view.as_ref(ctx).model.lock();
                match key {
                    Key::Up => EscCodes::build_escape_sequence(&*model, &[EscCodes::ARROW_UP]),
                    Key::Down => EscCodes::build_escape_sequence(&*model, &[EscCodes::ARROW_DOWN]),
                    Key::Enter => b"\r".to_vec(),
                }
            };
            view.update(ctx, |view, ctx| view.write_to_pty(bytes, ctx));
        }
        ctx.spawn(warpui::r#async::Timer::after(KEY_STEP), move |me, _, ctx| {
            me.step_claude_answer(terminal, target, answer, ctx);
        });
    }

    /// The terminal a notification points at, if it is still shown in its tab.
    fn claude_terminal(
        &self,
        terminal: EntityId,
        target: &NoticeTarget,
        ctx: &ViewContext<Self>,
    ) -> Option<warpui::ViewHandle<crate::terminal::TerminalView>> {
        let tab = self
            .tabs
            .iter()
            .find(|tab| tab.pane_group.id() == target.pane_group_id)?;
        super::vertical_tabs::tab_terminal_views(tab.pane_group.as_ref(ctx), ctx)
            .into_iter()
            .find(|(pane_id, view)| *pane_id == target.pane_id && view.id() == terminal)
            .map(|(_, view)| view)
    }

    /// A refused answer: nothing was typed, and a notification of its own says why.
    fn claude_notice_refused(
        &self,
        terminal: EntityId,
        target: &NoticeTarget,
        reason: &str,
        ctx: &mut ViewContext<Self>,
    ) {
        let category = claude_waits::notify::OPEN_ONLY;
        self.send_claude_notice(terminal, target, next_notice_id(), category, reason.to_owned(), ctx);
    }
}


/// Sessions any window of this app opened a tab for recently (`tmux_adopt::AdoptGuard`).
#[cfg(not(target_family = "wasm"))]
static TMUX_ADOPT_GUARD: std::sync::LazyLock<std::sync::Mutex<tmux_adopt::AdoptGuard>> =
    std::sync::LazyLock::new(Default::default);

#[cfg(not(target_family = "wasm"))]
impl Workspace {
    /// Looks for sessions another tool marked with `@warp_adopt` and gives each a tab here, if
    /// this window is the one that adopts.
    fn poll_tmux_adopt(&mut self, ctx: &mut ViewContext<Self>) {
        if !self.is_tmux_adopter(ctx) {
            return;
        }
        let shells = self.tab_shell_pids(ctx);
        ctx.spawn(
            async move {
                tokio::task::spawn_blocking(move || {
                    let candidates = tmux_agents::adoptable_sessions();
                    // Positions of the tabs already here, only needed when something is adopted.
                    let sessions = if candidates.is_empty() || shells.is_empty() {
                        Default::default()
                    } else {
                        tmux_agents::tmux_client_sessions()
                    };
                    (candidates, shells, sessions)
                })
                .await
            },
            |me, read, ctx| {
                let Ok((candidates, shells, sessions)) = read else {
                    return;
                };
                let mut tab_sessions = std::collections::HashMap::new();
                for (pane_group_id, pid) in shells {
                    if let Some(session) = sessions.get(&pid) {
                        tab_sessions
                            .entry(pane_group_id)
                            .or_insert_with(|| session.clone());
                    }
                }
                me.adopt_tmux_sessions(candidates, &tab_sessions, tmux_agents::unset_adopt, ctx);
            },
        );
    }

    /// Each tab's terminal shells, keyed by the tab.
    fn tab_shell_pids(&self, ctx: &warpui::AppContext) -> Vec<(EntityId, u32)> {
        self.tabs
            .iter()
            .flat_map(|tab| {
                let pane_group_id = tab.pane_group.id();
                super::vertical_tabs::tab_terminal_shell_pids(tab.pane_group.as_ref(ctx), ctx)
                    .into_iter()
                    .map(move |(_, pid)| (pane_group_id, pid))
            })
            .collect()
    }

    /// Whether this window adopts sessions: of the framed windows, the one with the most tabs,
    /// the frontmost on a tie, so two windows never open a tab for the same session.
    fn is_tmux_adopter(&self, ctx: &warpui::AppContext) -> bool {
        let mut order = ctx.windows().ordered_window_ids();
        for window_id in ctx.window_ids() {
            if !order.contains(&window_id) {
                order.push(window_id);
            }
        }
        let mut windows: Vec<_> = order
            .into_iter()
            .filter_map(|window_id| {
                let workspaces = ctx.views_of_type::<Workspace>(window_id)?;
                let framed: Vec<_> = workspaces
                    .iter()
                    .map(|workspace| workspace.as_ref(ctx))
                    .filter(|workspace| workspace.uses_framed_sidebar_layout(ctx))
                    .collect();
                (!framed.is_empty()).then(|| {
                    let tabs = framed.iter().map(|workspace| workspace.tabs.len()).sum();
                    (window_id, tabs)
                })
            })
            .collect();
        // The framework takes the view being updated out of its window's view map, so this
        // workspace is missing from views_of_type above; without counting it here no window
        // ever adopted.
        let own_tabs = self.uses_framed_sidebar_layout(ctx).then(|| self.tabs.len());
        tmux_adopt::count_own_window(&mut windows, self.window_id, own_tabs);
        tmux_adopt::pick_adopter(&windows) == Some(self.window_id)
    }

    /// Opens one background tab per candidate not claimed yet, places it by its group and
    /// position, and only then calls `unset_adopt` for it, so a crash in between leaves the
    /// session marked for the next start. Nothing is activated or focused.
    pub(in crate::workspace::view) fn adopt_tmux_sessions(
        &mut self,
        candidates: Vec<tmux_adopt::Candidate>,
        tab_sessions: &std::collections::HashMap<EntityId, tmux_agents::ClientSession>,
        mut unset_adopt: impl FnMut(&str),
        ctx: &mut ViewContext<Self>,
    ) {
        // The setting or the layout may have changed while tmux was read.
        if candidates.is_empty() || !self.uses_framed_sidebar_layout(ctx) || !tmux_sync_enabled(ctx)
        {
            return;
        }
        // Focus suppression is one window app-wide and a tab drag across windows holds it; adopt
        // at the next poll instead (nothing is claimed yet).
        if crate::workspace::cross_window_tab_drag::CrossWindowTabDrag::as_ref(ctx).is_active() {
            return;
        }
        // A new terminal queues focus for itself; the tab opens behind whatever the user is in.
        // Focus effects run after this update, so the suppression is lifted one tick later.
        ctx.set_suppress_focus_for_window(Some(self.window_id));
        let now = std::time::Instant::now();
        let mut adopted = false;
        for candidate in candidates {
            let Some(command) = tmux_adopt::attach_command(&candidate.id) else {
                continue;
            };
            let claimed = TMUX_ADOPT_GUARD
                .lock()
                .is_ok_and(|mut guard| guard.claim(&candidate.id, now));
            if !claimed {
                continue;
            }
            let target = tmux_adopt::adopt_index(
                &self.layout_tabs(tab_sessions),
                candidate.group.as_deref(),
                candidate.pos,
            );
            let index = self.add_background_command_tab(command, ctx);
            if let Some(group) = &candidate.group {
                self.put_tab_in_named_group(index, group);
            }
            self.hop_tab_to_index(index, target.min(index), ctx);
            unset_adopt(&candidate.id);
            adopted = true;
        }
        if adopted {
            self.keep_grouped_tabs_first(ctx);
            ctx.dispatch_global_action("workspace:save_app", ());
            ctx.notify();
        }
        ctx.spawn(async {}, |_, _, ctx| {
            ctx.set_suppress_focus_for_window(None)
        });
    }
}

/// A tab renamed by hand, waiting for its tmux session to carry the new name.
#[cfg(not(target_family = "wasm"))]
pub(super) struct TmuxRenameSettle {
    /// The session's name before the rename, if a poll had seen it.
    old_session_name: Option<String>,
    started: std::time::Instant,
    warned: bool,
}

/// Without a known old session name, how long the typed title wins over the session name.
#[cfg(not(target_family = "wasm"))]
const TMUX_RENAME_SETTLE: std::time::Duration = std::time::Duration::from_secs(5);

/// After this long without the session taking the new name, the failure is logged.
#[cfg(not(target_family = "wasm"))]
const TMUX_RENAME_WARN: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(not(target_family = "wasm"))]
mod tmux_agents {
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::process::Command;

    const AGENT_PROCESS_NAMES: [&str; 2] = ["claude", "codex"];

    /// How far below a pane's own process an agent may sit (e.g. pane shell, then agent).
    const AGENT_SEARCH_DEPTH: usize = 3;

    /// Pids of shells whose tmux client is attached to a session with an agent running in one of
    /// its panes. Blocking (runs `tmux` and `ps`), so call it off the UI thread. Any failure, such
    /// as no tmux or no server, yields an empty set.
    pub(super) fn tmux_agent_shell_pids() -> HashSet<u32> {
        let tmux = tmux_binary();
        let Some(panes) = run(tmux, &["list-panes", "-a", "-F", "#{session_name}\t#{pane_pid}"])
        else {
            return HashSet::new();
        };
        let Some(processes) = process_listing() else {
            return HashSet::new();
        };
        let table = ProcessTable::parse(&processes);

        // tmux reports Claude Code's pane command as its version string, so match on `ps` names.
        let agent_sessions: HashSet<&str> = panes
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .filter(|(_, pane_pid)| {
                pane_pid
                    .trim()
                    .parse()
                    .is_ok_and(|pid| table.runs_agent(pid, AGENT_SEARCH_DEPTH))
            })
            .map(|(session, _)| session)
            .collect();
        if agent_sessions.is_empty() {
            return HashSet::new();
        }

        let Some(clients) = run(tmux, &["list-clients", "-F", "#{client_pid}\t#{session_name}"])
        else {
            return HashSet::new();
        };
        let mut shells = HashSet::new();
        for (client_pid, session) in clients.lines().filter_map(|line| line.split_once('\t')) {
            if !agent_sessions.contains(session) {
                continue;
            }
            let Ok(client_pid) = client_pid.trim().parse::<u32>() else {
                continue;
            };
            // The client is normally a child of the tab's shell; after `exec tmux` it is the shell.
            shells.insert(client_pid);
            if let Some(&parent) = table.parent.get(&client_pid) {
                shells.insert(parent);
            }
        }
        shells
    }

    /// The tmux session a tab's client is attached to, with the group the session remembers.
    #[derive(Clone)]
    pub(in crate::workspace::view) struct ClientSession {
        pub id: String,
        pub name: String,
        /// The folder of the session's active pane, for the card header.
        pub path: String,
        pub group: String,
        /// @warp_inactive and @warp_unread, set to "1" by the app or another tmux client.
        pub inactive: bool,
        pub unread: bool,
        /// @warp_pos: the tab's position within its sidebar section (`super::tmux_layout`).
        pub pos: Option<u32>,
        /// @warp_pin: when the session was pinned to the sidebar (`super::sidebar_pins`).
        pub pin: Option<u64>,
    }

    /// Client shells (and the clients themselves, for `exec tmux`) mapped to their session.
    /// Blocking; call it off the UI thread. Any failure yields an empty map.
    pub(in crate::workspace::view) fn tmux_client_sessions() -> HashMap<u32, ClientSession> {
        let mut sessions = HashMap::new();
        let Some(clients) = run(
            tmux_binary(),
            &[
                "list-clients",
                "-F",
                "#{client_pid}\t#{session_id}\t#{session_name}\t#{pane_current_path}\t#{@warp_group}\t#{@warp_inactive}\t#{@warp_unread}\t#{@warp_pos}\t#{@warp_pin}",
            ],
        ) else {
            return sessions;
        };
        let Some(processes) = process_listing() else {
            return sessions;
        };
        let table = ProcessTable::parse(&processes);
        for line in clients.lines() {
            let mut fields = line.split('\t');
            let (Some(pid), Some(id), Some(name), Some(path)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let Ok(pid) = pid.trim().parse::<u32>() else {
                continue;
            };
            let session = ClientSession {
                id: id.to_owned(),
                name: name.to_owned(),
                path: path.to_owned(),
                group: fields.next().unwrap_or_default().to_owned(),
                inactive: fields.next() == Some("1"),
                unread: fields.next() == Some("1"),
                pos: fields.next().and_then(|pos| pos.trim().parse().ok()),
                pin: fields.next().and_then(super::sidebar_pins::parse),
            };
            if let Some(&parent) = table.parent.get(&pid) {
                sessions.insert(parent, session.clone());
            }
            sessions.insert(pid, session);
        }
        sessions
    }

    /// Renames the session whose client is `shell_pid` (or a child of it). With a script:
    /// `<script> rename <shell_pid> <group> <title>`; without: `tmux rename-session` to the title.
    /// Detached; the lookup runs off the UI thread.
    pub(in crate::workspace::view) fn rename_session(
        script: Option<PathBuf>,
        shell_pid: u32,
        group: &str,
        title: &str,
    ) {
        if let Some(script) = script {
            spawn_detached(Command::new(script).args([
                "rename",
                &shell_pid.to_string(),
                group,
                title,
            ]));
            return;
        }
        let title = title.to_owned();
        std::thread::spawn(move || {
            if let Some(session) = tmux_client_sessions().remove(&shell_pid) {
                spawn_detached(Command::new(tmux_binary()).args([
                    "rename-session",
                    "-t",
                    &session.id,
                    &title,
                ]));
            }
        });
    }

    /// Stores a tab's group on its session. With a script: `<script> group <session_id> <old>
    /// <new>`, so it can also rename the session; without: `@warp_group` is set (or unset for
    /// no group) directly.
    pub(in crate::workspace::view) fn set_session_group(
        script: Option<PathBuf>,
        session_id: &str,
        old: &str,
        new: &str,
    ) {
        if let Some(script) = script {
            spawn_detached(Command::new(script).args(["group", session_id, old, new]));
        } else if new.is_empty() {
            spawn_detached(Command::new(tmux_binary()).args([
                "set-option",
                "-u",
                "-t",
                session_id,
                "@warp_group",
            ]));
        } else {
            spawn_detached(Command::new(tmux_binary()).args([
                "set-option",
                "-t",
                session_id,
                "@warp_group",
                new,
            ]));
        }
    }

    /// `@warp_layout_rev` and `@warp_groups` (one `show-options -gqv` each). `None` when tmux
    /// cannot be asked (no server); an unset option reads as `None` / empty. Blocking.
    pub(in crate::workspace::view) fn read_layout_globals() -> Option<(Option<u64>, Vec<String>)> {
        let rev = run(tmux_binary(), &["show-options", "-gqv", "@warp_layout_rev"])?;
        let groups = run(tmux_binary(), &["show-options", "-gqv", "@warp_groups"])?;
        Some((
            super::tmux_layout::parse_rev(&rev),
            super::tmux_layout::parse_group_list(&groups),
        ))
    }

    /// Writes the order in one tmux call (positions, then the group list, then the new rev) if
    /// tmux still holds `based_on`; returns the rev written. Blocking. The check and the write
    /// are two tmux calls, so a writer that slips in between them by milliseconds can still be
    /// overwritten; both then bumped the rev, and the next poll settles on the newer one.
    pub(in crate::workspace::view) fn write_layout(
        based_on: Option<u64>,
        positions: &[(String, u32)],
        groups: Option<&str>,
    ) -> Option<u64> {
        let current = run(tmux_binary(), &["show-options", "-gqv", "@warp_layout_rev"])?;
        let rev = super::tmux_layout::next_rev(super::tmux_layout::parse_rev(&current), based_on)?;
        let mut args: Vec<String> = Vec::new();
        for (session_id, pos) in positions {
            args.extend(["set-option", "-t", session_id, "@warp_pos"].map(str::to_owned));
            args.push(pos.to_string());
            args.push(";".to_owned());
        }
        if let Some(groups) = groups {
            args.extend(["set-option", "-g", "@warp_groups", groups].map(str::to_owned));
            args.push(";".to_owned());
        }
        args.extend(["set-option", "-g", "@warp_layout_rev"].map(str::to_owned));
        args.push(rev.to_string());
        let status = Command::new(tmux_binary())
            .args(&args)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        status.success().then_some(rev)
    }

    /// Sessions waiting to be adopted (`super::tmux_adopt`). Blocking; any failure, such as no
    /// tmux server, yields none.
    pub(in crate::workspace::view) fn adoptable_sessions() -> Vec<super::tmux_adopt::Candidate> {
        run(
            tmux_binary(),
            &["list-sessions", "-F", super::tmux_adopt::LIST_FORMAT],
        )
        .map(|output| super::tmux_adopt::parse_candidates(&output))
        .unwrap_or_default()
    }

    /// The dialog each terminal's Claude Code waits on, for terminals (`(terminal view, shell
    /// pid)`) whose tmux session has a pane where a live Claude Code's session file says
    /// "waiting". Blocking. Without any waiting file, tmux is not asked at all.
    pub(in crate::workspace::view) fn claude_waits_by_terminal(
        terminals: &[(warpui::EntityId, u32)],
    ) -> HashMap<warpui::EntityId, super::claude_waits::Wait> {
        let Some(dir) = dirs::home_dir().map(|home| home.join(".claude").join("sessions")) else {
            return HashMap::new();
        };
        let waits: Vec<_> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .filter_map(|json| super::claude_waits::parse_session_file(&json))
            .collect();
        if waits.is_empty() {
            return HashMap::new();
        }
        let Some(panes) = run(
            tmux_binary(),
            &["list-panes", "-a", "-F", "#{pane_id} #{session_id}"],
        ) else {
            return HashMap::new();
        };
        let by_session = super::claude_waits::waits_by_session(
            waits,
            &super::claude_waits::parse_pane_sessions(&panes),
            pid_alive,
        );
        let clients = tmux_client_sessions();
        terminals
            .iter()
            .filter_map(|(terminal_view_id, pid)| {
                let wait = by_session.get(&clients.get(pid)?.id)?;
                Some((*terminal_view_id, wait.clone()))
            })
            .collect()
    }

    fn pid_alive(pid: u32) -> bool {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

        let pid = Pid::from_u32(pid);
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        system.process(pid).is_some()
    }

    /// Clears a session's `@warp_adopt` once its tab exists.
    pub(in crate::workspace::view) fn unset_adopt(session_id: &str) {
        set_session_flag(session_id, "@warp_adopt", false);
    }

    /// Sets (`on`) or removes a session's flag option, detached and reaped off the UI thread.
    pub(in crate::workspace::view) fn set_session_flag(session_id: &str, option: &str, on: bool) {
        set_session_option(session_id, option, on.then_some("1"));
    }

    /// Sets a session option to `value`, or unsets it for `None`. Detached.
    pub(in crate::workspace::view) fn set_session_option(
        session_id: &str,
        option: &str,
        value: Option<&str>,
    ) {
        let mut command = Command::new(tmux_binary());
        match value {
            Some(value) => command.args(["set-option", "-t", session_id, option, value]),
            None => command.args(["set-option", "-u", "-t", session_id, option]),
        };
        spawn_detached(&mut command);
    }

    /// Starts `command` without a terminal and reaps it on its own thread. The app may have been
    /// started from inside tmux; the command must not inherit that.
    fn spawn_detached(command: &mut Command) {
        let child = command
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match child {
            Ok(mut child) => {
                std::thread::spawn(move || child.wait());
            }
            Err(err) => log::warn!("could not run {:?}: {err}", command.get_program()),
        }
    }

    // The app does not inherit a login shell's PATH, so look in the usual install locations.
    #[cfg(not(windows))]
    fn tmux_binary() -> &'static str {
        ["/opt/homebrew/bin/tmux", "/usr/local/bin/tmux"]
            .into_iter()
            .find(|path| std::path::Path::new(path).exists())
            .unwrap_or("tmux")
    }

    /// psmux, the Windows tmux, which winget links into `WinGet\Links` as `tmux.exe`.
    #[cfg(windows)]
    fn tmux_binary() -> &'static str {
        static BINARY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        BINARY.get_or_init(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(|dir| PathBuf::from(dir).join(r"Microsoft\WinGet\Links\tmux.exe"))
                .filter(|path| path.exists())
                .map_or_else(|| "tmux".to_owned(), |path| path.to_string_lossy().into_owned())
        })
    }

    /// Every process as a `pid ppid name` line, the shape `ProcessTable::parse` reads.
    #[cfg(not(windows))]
    fn process_listing() -> Option<String> {
        run("/bin/ps", &["-A", "-o", "pid=,ppid=,comm="])
    }

    #[cfg(windows)]
    fn process_listing() -> Option<String> {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        let listing = system
            .processes()
            .iter()
            .map(|(pid, process)| {
                let parent = process.parent().map_or(0, |parent| parent.as_u32());
                format!("{} {parent} {}", pid.as_u32(), process.name().to_string_lossy())
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some(listing)
    }

    fn run(program: &str, args: &[&str]) -> Option<String> {
        let output = Command::new(program).args(args).output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    struct ProcessTable {
        parent: HashMap<u32, u32>,
        children: HashMap<u32, Vec<u32>>,
        name: HashMap<u32, String>,
    }

    impl ProcessTable {
        fn parse(ps_output: &str) -> Self {
            let mut table = Self {
                parent: HashMap::new(),
                children: HashMap::new(),
                name: HashMap::new(),
            };
            for line in ps_output.lines() {
                let mut fields = line.split_whitespace();
                let (Some(pid), Some(ppid)) = (fields.next(), fields.next()) else {
                    continue;
                };
                let (Ok(pid), Ok(ppid)) = (pid.parse::<u32>(), ppid.parse::<u32>()) else {
                    continue;
                };
                // `comm` is the last column and may contain spaces.
                let command = fields.collect::<Vec<_>>().join(" ");
                let base_name = command.rsplit(['/', '\\']).next().unwrap_or_default();
                let base_name = base_name.strip_suffix(".exe").unwrap_or(base_name).to_owned();
                table.parent.insert(pid, ppid);
                table.children.entry(ppid).or_default().push(pid);
                table.name.insert(pid, base_name);
            }
            table
        }

        fn runs_agent(&self, pid: u32, depth: usize) -> bool {
            if self
                .name
                .get(&pid)
                .is_some_and(|name| AGENT_PROCESS_NAMES.contains(&name.as_str()))
            {
                return true;
            }
            depth > 0
                && self.children.get(&pid).is_some_and(|children| {
                    children
                        .iter()
                        .any(|&child| self.runs_agent(child, depth - 1))
                })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn finds_an_agent_below_the_pane_shell() {
            let table = ProcessTable::parse("10 1 zsh\n11 10 /usr/local/bin/claude\n");
            assert!(table.runs_agent(10, 1));
            assert!(!table.runs_agent(10, 0));
        }

        #[test]
        fn windows_names_match_without_path_or_exe() {
            let table =
                ProcessTable::parse("20 1 pwsh.exe\n21 20 claude.exe\n22 20 C:\\bin\\codex.exe\n");
            assert!(table.runs_agent(21, 0));
            assert!(table.runs_agent(22, 0));
            assert!(!table.runs_agent(20, 0));
        }

        #[test]
        fn names_with_spaces_and_junk_lines_are_handled() {
            let table = ProcessTable::parse("not a line\n30 1 Some Tool.exe\n31 30 claude\n");
            assert_eq!(table.name.get(&30).map(String::as_str), Some("Some Tool"));
            assert!(table.runs_agent(30, 1));
        }
    }
}

/// The sidebar order as stored in tmux, so another tmux client (a phone bridge) can show and
/// change it and it survives a restart. Pure logic only; reading and writing tmux is in
/// `Workspace::sync_tmux_layout` and `tmux_agents`.
///
/// Contract with other writers:
/// - `@warp_pos` (session option): the tab's position within its section, an integer >= 0.
///   Ungrouped tabs ("Other") are their own section.
/// - `@warp_groups` (global): group names in sidebar order, one per line. It may hold empty
///   groups and names this app does not know; a write keeps those at their place.
/// - `@warp_layout_rev` (global): whoever writes either of the above increments it by one, and
///   only writes if the rev it read is still current.
///
/// How this can go wrong, and the test that pins each case:
/// 1. A poll that started before the app's own write reads the old order and reverts a drag:
///    `a_stale_or_known_rev_is_not_adopted` (freshness is `read_sees_write`).
/// 2. A write drops group names the app does not know (another machine's, empty groups):
///    `unknown_group_names_keep_their_place`, `a_new_app_group_goes_after_its_predecessor`.
/// 3. The app writes over a newer change from another client (rev race):
///    `a_write_only_lands_on_the_rev_it_read`.
/// 4. Tabs without a tmux session (no pos) get shuffled on adopt:
///    `tabs_without_a_pos_keep_their_order_after_the_positioned_ones`.
/// 5. Pinned tabs leave their pinned region on adopt: `pinned_tabs_never_move`.
/// 6. Sections come out in the wrong order (unknown names, groups missing from the list, "Other"
///    not last): `sections_follow_the_list_and_other_stays_last`.
/// 7. The app writes layout options on a tmux server where none of its tabs is attached (a test
///    app polling someone else's default server): `nothing_is_written_without_an_attached_tab`.
pub(in crate::workspace::view) mod tmux_layout {
    /// One sidebar tab as the layout sees it.
    #[derive(Clone, Debug, PartialEq)]
    pub(in crate::workspace::view) struct LayoutTab {
        pub pinned: bool,
        /// Its group's name; `None` for "Other" (and for a group without a name).
        pub group: Option<String>,
        /// `@warp_pos` of its tmux session; `None` without a session or without the option.
        pub pos: Option<u32>,
    }

    /// Parses `@warp_groups`: one name per line, blank lines skipped.
    pub(in crate::workspace::view) fn parse_group_list(value: &str) -> Vec<String> {
        value
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// Parses `@warp_layout_rev`; unset or garbage reads as `None`.
    pub(in crate::workspace::view) fn parse_rev(value: &str) -> Option<u64> {
        value.trim().parse().ok()
    }

    /// Whether a read's rev is another writer's change the app has not adopted yet. A read that
    /// may predate the app's own last write (`fresh == false`) never counts, so it cannot revert
    /// that write.
    pub(in crate::workspace::view) fn should_adopt(
        read_rev: Option<u64>,
        agreed_rev: Option<u64>,
        fresh: bool,
    ) -> bool {
        fresh && read_rev.is_some_and(|read| agreed_rev.is_none_or(|agreed| read > agreed))
    }

    /// The rev a write sets, or `None` when tmux no longer holds the rev the write was based on
    /// (someone else wrote in between; the next poll adopts their change instead).
    pub(in crate::workspace::view) fn next_rev(
        current_in_tmux: Option<u64>,
        based_on: Option<u64>,
    ) -> Option<u64> {
        (current_in_tmux == based_on).then(|| based_on.map_or(1, |rev| rev + 1))
    }

    /// Whether the app may write the layout options at all: only while at least one of its tabs
    /// is attached to a session on that tmux server. Otherwise the server is someone else's (a
    /// test app polling the default server, a window with no tmux tabs) and the app has no
    /// order to store there.
    pub(in crate::workspace::view) fn may_write(attached_tabs: usize) -> bool {
        attached_tabs > 0
    }

    /// The app's named groups in sidebar order (first appearance), pinned tabs included.
    pub(in crate::workspace::view) fn app_group_order(tabs: &[LayoutTab]) -> Vec<String> {
        let mut order: Vec<String> = Vec::new();
        for name in tabs.iter().filter_map(|tab| tab.group.as_ref()) {
            if !order.contains(name) {
                order.push(name.clone());
            }
        }
        order
    }

    /// Each unpinned tab's position within its section (0, 1, ...); `None` for pinned tabs,
    /// which keep their own region and are not part of the stored order.
    pub(in crate::workspace::view) fn section_positions(tabs: &[LayoutTab]) -> Vec<Option<u32>> {
        let mut counts: Vec<(Option<&String>, u32)> = Vec::new();
        tabs.iter()
            .map(|tab| {
                if tab.pinned {
                    return None;
                }
                let section = tab.group.as_ref();
                match counts.iter_mut().find(|(key, _)| *key == section) {
                    Some((_, count)) => {
                        *count += 1;
                        Some(*count - 1)
                    }
                    None => {
                        counts.push((section, 1));
                        Some(0)
                    }
                }
            })
            .collect()
    }

    /// `@warp_groups` to write: the app's groups in the app's order, every name the app does not
    /// know left at its place. The app's groups fill the slots its known names held; a group the
    /// list does not have yet goes right after the app group before it (or first).
    pub(in crate::workspace::view) fn merge_group_order(
        tmux: &[String],
        app: &[String],
    ) -> Vec<String> {
        let mut known_in_list = app.iter().filter(|name| tmux.contains(name));
        let mut merged: Vec<String> = tmux
            .iter()
            .map(|name| {
                if app.contains(name) {
                    known_in_list.next().cloned().unwrap_or_else(|| name.clone())
                } else {
                    name.clone()
                }
            })
            .collect();
        for (index, name) in app.iter().enumerate() {
            if merged.contains(name) {
                continue;
            }
            let at = index
                .checked_sub(1)
                .and_then(|before| merged.iter().position(|entry| *entry == app[before]))
                .map_or(0, |position| position + 1);
            merged.insert(at, name.clone());
        }
        merged
    }

    /// The new tab order (indices into `tabs`) for an adopted layout. Pinned tabs keep their
    /// indices. The rest is sorted into sections: groups in `group_order` (unknown names
    /// skipped), groups missing from the list after the listed ones in their current order, and
    /// "Other" last. Within a section, tabs with a pos by pos, then tabs without one in their
    /// current order.
    pub(in crate::workspace::view) fn adopted_order(
        tabs: &[LayoutTab],
        group_order: &[String],
    ) -> Vec<usize> {
        let current_groups = app_group_order(tabs);
        let section_rank = |group: &Option<String>| -> (u8, usize) {
            match group {
                None => (2, 0),
                Some(name) => match group_order.iter().position(|listed| listed == name) {
                    Some(position) => (0, position),
                    None => (
                        1,
                        current_groups
                            .iter()
                            .position(|known| known == name)
                            .unwrap_or(0),
                    ),
                },
            }
        };
        let mut unpinned: Vec<usize> = (0..tabs.len()).filter(|&i| !tabs[i].pinned).collect();
        // Stable: equal keys (no pos, or the same pos) keep their current order.
        unpinned.sort_by_key(|&i| {
            let tab = &tabs[i];
            (
                section_rank(&tab.group),
                tab.pos.is_none(),
                tab.pos.unwrap_or(0),
            )
        });
        let mut unpinned = unpinned.into_iter();
        (0..tabs.len())
            .map(|i| {
                if tabs[i].pinned {
                    i
                } else {
                    unpinned.next().expect("one unpinned tab per unpinned slot")
                }
            })
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn tab(group: Option<&str>, pos: Option<u32>) -> LayoutTab {
            LayoutTab {
                pinned: false,
                group: group.map(str::to_owned),
                pos,
            }
        }

        fn names(list: &[&str]) -> Vec<String> {
            list.iter().map(|name| (*name).to_owned()).collect()
        }

        #[test]
        fn a_stale_or_known_rev_is_not_adopted() {
            // The phone's change (rev 6) arrives with a fresh read: adopt.
            assert!(should_adopt(Some(6), Some(5), true));
            // The same rev from a read that may predate our own write: never.
            assert!(!should_adopt(Some(6), Some(5), false));
            // Our own rev, or an older one, changes nothing.
            assert!(!should_adopt(Some(5), Some(5), true));
            assert!(!should_adopt(Some(4), Some(5), true));
            // Nothing stored yet: nothing to adopt; after a restart any stored rev is news.
            assert!(!should_adopt(None, None, true));
            assert!(should_adopt(Some(1), None, true));
        }

        #[test]
        fn nothing_is_written_without_an_attached_tab() {
            // A test app polling the default server, where it has no tabs: even with the rev
            // unset (the "first start" case) it writes nothing there.
            assert!(!may_write(0));
            assert!(may_write(1));
        }

        #[test]
        fn a_write_only_lands_on_the_rev_it_read() {
            assert_eq!(next_rev(None, None), Some(1));
            assert_eq!(next_rev(Some(5), Some(5)), Some(6));
            // The phone wrote rev 6 after our read of 5: our write is dropped.
            assert_eq!(next_rev(Some(6), Some(5)), None);
            assert_eq!(next_rev(Some(3), None), None);
        }

        #[test]
        fn unknown_group_names_keep_their_place() {
            // "laptop" and "empty" are unknown here; the app swapped "a" and "b".
            let tmux = names(&["a", "laptop", "b", "empty"]);
            assert_eq!(
                merge_group_order(&tmux, &names(&["b", "a"])),
                names(&["b", "laptop", "a", "empty"])
            );
            // Unchanged app order: the list comes back as it was.
            assert_eq!(merge_group_order(&tmux, &names(&["a", "b"])), tmux);
            // The app has no groups at all: every name stays.
            assert_eq!(merge_group_order(&tmux, &[]), tmux);
        }

        #[test]
        fn a_new_app_group_goes_after_its_predecessor() {
            let tmux = names(&["a", "laptop", "b"]);
            assert_eq!(
                merge_group_order(&tmux, &names(&["a", "new", "b"])),
                names(&["a", "new", "laptop", "b"])
            );
            assert_eq!(
                merge_group_order(&tmux, &names(&["first", "a", "b"])),
                names(&["first", "a", "laptop", "b"])
            );
            assert_eq!(
                merge_group_order(&[], &names(&["x", "y"])),
                names(&["x", "y"])
            );
        }

        #[test]
        fn sections_follow_the_list_and_other_stays_last() {
            let tabs = [
                tab(None, Some(0)),
                tab(Some("a"), Some(0)),
                tab(Some("b"), Some(0)),
                tab(Some("c"), Some(0)),
            ];
            // "b" first, "zzz" unknown, "a" listed, "c" not listed: after the listed ones.
            assert_eq!(
                adopted_order(&tabs, &names(&["b", "zzz", "a"])),
                [2, 1, 3, 0]
            );
        }

        #[test]
        fn tabs_without_a_pos_keep_their_order_after_the_positioned_ones() {
            let tabs = [
                tab(Some("a"), None), // no tmux session
                tab(Some("a"), Some(1)),
                tab(Some("a"), None), // no tmux session
                tab(Some("a"), Some(0)),
            ];
            assert_eq!(adopted_order(&tabs, &names(&["a"])), [3, 1, 0, 2]);
            // Nothing positioned: nothing moves.
            let plain = [tab(None, None), tab(None, None), tab(None, None)];
            assert_eq!(adopted_order(&plain, &[]), [0, 1, 2]);
        }

        #[test]
        fn pinned_tabs_never_move() {
            let mut pinned = tab(Some("a"), Some(9));
            pinned.pinned = true;
            let tabs = [
                pinned.clone(),
                tab(None, Some(1)),
                tab(None, Some(0)),
                pinned,
            ];
            assert_eq!(adopted_order(&tabs, &[]), [0, 2, 1, 3]);
            assert_eq!(section_positions(&tabs), [None, Some(0), Some(1), None]);
        }

        #[test]
        fn positions_count_within_each_section() {
            let tabs = [
                tab(Some("a"), None),
                tab(Some("a"), None),
                tab(None, None),
                tab(Some("b"), None),
                tab(None, None),
            ];
            assert_eq!(
                section_positions(&tabs),
                [Some(0), Some(1), Some(0), Some(0), Some(1)]
            );
            assert_eq!(app_group_order(&tabs), names(&["a", "b"]));
        }

        #[test]
        fn group_list_and_rev_parse_loosely() {
            assert_eq!(parse_group_list("a\n\n b \nc\n"), names(&["a", "b", "c"]));
            assert_eq!(parse_rev(" 7\n"), Some(7));
            assert_eq!(parse_rev(""), None);
            assert_eq!(parse_rev("x"), None);
        }
    }
}

/// Adopting tmux sessions that another tool (the phone bridge) started without a client. Pure
/// logic only; reading tmux and opening the tab is in `Workspace::poll_tmux_adopt`.
///
/// Contract with the bridge: it creates the session with `tmux new-session -d` and then sets the
/// session option `@warp_adopt` to "1". With the tmux integration on, the adopting window opens
/// one background tab per such session that has no client, running the same attach command the
/// restart launch config uses, places it by `@warp_group` / `@warp_pos`, and only then unsets
/// `@warp_adopt`.
///
/// How this can go wrong, and the test that pins each case:
/// 1. A session with a client (a tab already attached, or a terminal elsewhere) gets a second
///    tab: `only_unattached_sessions_marked_for_adoption_are_candidates`.
/// 2. A poll that read before the unset landed, or while the attach still connects, opens a
///    duplicate: `a_session_is_claimed_once_until_the_guard_expires`.
/// 3. A malformed session id reaches the shell: `only_plain_session_ids_get_an_attach_command`.
/// 4. The tab lands in the wrong section or place: `adopt_index_*`.
/// 5. Two windows both adopt: `the_window_with_most_tabs_adopts_and_the_frontmost_wins_ties`.
///
/// Focus, the integration switch and the unset-after-tab order are covered in `view_tests.rs`.
#[cfg(not(target_family = "wasm"))]
pub(in crate::workspace::view) mod tmux_adopt {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use super::tmux_layout::LayoutTab;

    /// The `tmux list-sessions -F` format `parse_candidates` reads.
    pub(in crate::workspace::view) const LIST_FORMAT: &str =
        "#{session_id}\t#{session_attached}\t#{@warp_adopt}\t#{@warp_group}\t#{@warp_pos}";

    /// How long a session stays claimed after its tab opened: long enough for the unset to land
    /// and the attach to connect, so later polls neither see it unattached nor still marked.
    const ADOPT_GUARD: Duration = Duration::from_secs(60);

    /// A session waiting for a tab.
    #[derive(Clone, Debug, PartialEq)]
    pub(in crate::workspace::view) struct Candidate {
        pub id: String,
        pub group: Option<String>,
        pub pos: Option<u32>,
    }

    /// Sessions marked `@warp_adopt` = "1" that no client is attached to, in tmux's order.
    /// `#{session_attached}` is a client count, so only "0" counts as unattached.
    pub(in crate::workspace::view) fn parse_candidates(output: &str) -> Vec<Candidate> {
        output
            .lines()
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let id = fields.next()?;
                let attached = fields.next()?;
                let adopt = fields.next()?;
                if adopt.trim() != "1" || attached.trim() != "0" || !is_session_id(id) {
                    return None;
                }
                let group = fields.next().unwrap_or_default().trim();
                Some(Candidate {
                    id: id.to_owned(),
                    group: (!group.is_empty()).then(|| group.to_owned()),
                    pos: fields.next().and_then(|pos| pos.trim().parse().ok()),
                })
            })
            .collect()
    }

    /// tmux's own session ids look like `$12`; nothing else is ever put into a shell command.
    fn is_session_id(id: &str) -> bool {
        id.strip_prefix('$').is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
    }

    /// The command the tab's shell runs, the same one the restart launch config writes.
    pub(in crate::workspace::view) fn attach_command(session_id: &str) -> Option<String> {
        is_session_id(session_id).then(|| format!("tmux attach -t '{session_id}'"))
    }

    /// Sessions this app opened a tab for recently, across all windows.
    #[derive(Default)]
    pub(in crate::workspace::view) struct AdoptGuard {
        claimed: HashMap<String, Instant>,
    }

    impl AdoptGuard {
        /// True (and remembered) if `session_id` was not claimed within `ADOPT_GUARD` of `now`.
        pub(in crate::workspace::view) fn claim(&mut self, session_id: &str, now: Instant) -> bool {
            self.claimed
                .retain(|_, claimed_at| now.saturating_duration_since(*claimed_at) < ADOPT_GUARD);
            if self.claimed.contains_key(session_id) {
                return false;
            }
            self.claimed.insert(session_id.to_owned(), now);
            true
        }
    }

    /// Where a tab for a session in `group` at `pos` goes, as an index into `tabs` before the new
    /// tab is added: before the first tab of its section with an equal or higher position, else
    /// after the section's last tab. A section with no tabs yet yields `tabs.len()`; grouped-first
    /// ordering then moves the tab to the end of the grouped run, or it stays last in "Other".
    pub(in crate::workspace::view) fn adopt_index(
        tabs: &[LayoutTab],
        group: Option<&str>,
        pos: Option<u32>,
    ) -> usize {
        let section: Vec<usize> = tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| !tab.pinned && tab.group.as_deref() == group)
            .map(|(index, _)| index)
            .collect();
        let Some(&last) = section.last() else {
            return tabs.len();
        };
        pos.and_then(|pos| {
            section
                .iter()
                .copied()
                .find(|&index| tabs[index].pos.is_some_and(|other| other >= pos))
        })
        .unwrap_or(last + 1)
    }

    /// The window that adopts: the one with the most tabs; `windows` is front to back, so the
    /// frontmost wins a tie. `None` without windows.
    /// Adds the polling window's own tabs to the per-window counts it collected, which never
    /// include the workspace that is being updated. Windows keep their order (frontmost first).
    pub(in crate::workspace::view) fn count_own_window<W: Copy + PartialEq>(
        windows: &mut Vec<(W, usize)>,
        own: W,
        own_tabs: Option<usize>,
    ) {
        let Some(own_tabs) = own_tabs else {
            return;
        };
        match windows.iter_mut().find(|(window, _)| *window == own) {
            Some((_, tabs)) => *tabs += own_tabs,
            None => windows.push((own, own_tabs)),
        }
    }

    pub(in crate::workspace::view) fn pick_adopter<W: Copy>(windows: &[(W, usize)]) -> Option<W> {
        let most = windows.iter().map(|(_, tabs)| *tabs).max()?;
        windows
            .iter()
            .find(|(_, tabs)| *tabs == most)
            .map(|(window, _)| *window)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The polling window is never in its own view map, so before this it never adopted;
        /// with two windows each picked the other and neither adopted.
        #[test]
        fn the_polling_window_counts_itself() {
            let mut alone = Vec::<(u32, usize)>::new();
            count_own_window(&mut alone, 1, Some(10));
            assert_eq!(pick_adopter(&alone), Some(1));

            // Window 1 (10 tabs) polls and sees only window 2 (1 tab), and the reverse.
            let mut seen_by_1 = vec![(2u32, 1usize)];
            count_own_window(&mut seen_by_1, 1, Some(10));
            let mut seen_by_2 = vec![(1u32, 10usize)];
            count_own_window(&mut seen_by_2, 2, Some(1));
            assert_eq!(pick_adopter(&seen_by_1), Some(1));
            assert_eq!(pick_adopter(&seen_by_2), Some(1));

            // A window without the framed layout adds nothing.
            let mut none = Vec::<(u32, usize)>::new();
            count_own_window(&mut none, 1, None);
            assert_eq!(pick_adopter(&none), None);
        }

        fn tab(group: Option<&str>, pos: Option<u32>) -> LayoutTab {
            LayoutTab {
                pinned: false,
                group: group.map(str::to_owned),
                pos,
            }
        }

        #[test]
        fn only_unattached_sessions_marked_for_adoption_are_candidates() {
            let output = "$1\t0\t1\twork\t2\n\
                          $2\t1\t1\twork\t0\n\
                          $3\t2\t1\t\t\n\
                          $4\t0\t\t\t\n\
                          $5\t0\t1\t\t\n\
                          bad\t0\t1\t\t\n";
            assert_eq!(
                parse_candidates(output),
                vec![
                    Candidate {
                        id: "$1".to_owned(),
                        group: Some("work".to_owned()),
                        pos: Some(2),
                    },
                    Candidate {
                        id: "$5".to_owned(),
                        group: None,
                        pos: None,
                    },
                ]
            );
        }

        #[test]
        fn only_plain_session_ids_get_an_attach_command() {
            assert_eq!(
                attach_command("$12").as_deref(),
                Some("tmux attach -t '$12'")
            );
            assert_eq!(attach_command("$"), None);
            assert_eq!(attach_command("$1'; rm -rf ~; '"), None);
            assert_eq!(attach_command("work"), None);
        }

        #[test]
        fn a_session_is_claimed_once_until_the_guard_expires() {
            let start = Instant::now();
            let mut guard = AdoptGuard::default();
            assert!(guard.claim("$1", start));
            assert!(!guard.claim("$1", start + Duration::from_secs(10)));
            assert!(!guard.claim("$1", start + Duration::from_secs(59)));
            assert!(guard.claim("$2", start + Duration::from_secs(10)));
            assert!(guard.claim("$1", start + ADOPT_GUARD));
        }

        #[test]
        fn adopt_index_places_by_pos_within_the_group() {
            let tabs = [
                tab(Some("a"), Some(0)),
                tab(Some("b"), Some(0)),
                tab(Some("b"), Some(1)),
                tab(Some("b"), Some(3)),
                tab(None, Some(0)),
            ];
            assert_eq!(adopt_index(&tabs, Some("b"), Some(2)), 3);
            assert_eq!(adopt_index(&tabs, Some("b"), Some(0)), 1);
            // Past every position, or no position: end of the section.
            assert_eq!(adopt_index(&tabs, Some("b"), Some(9)), 4);
            assert_eq!(adopt_index(&tabs, Some("b"), None), 4);
        }

        #[test]
        fn adopt_index_without_a_group_goes_to_other() {
            let tabs = [tab(Some("a"), Some(0)), tab(None, Some(0)), tab(None, None)];
            assert_eq!(adopt_index(&tabs, None, None), 3);
            assert_eq!(adopt_index(&tabs, None, Some(0)), 1);
        }

        #[test]
        fn adopt_index_for_a_new_group_or_empty_section_appends() {
            let tabs = [tab(Some("a"), Some(0)), tab(None, None)];
            assert_eq!(adopt_index(&tabs, Some("new"), Some(0)), 2);
            assert_eq!(adopt_index(&[], None, Some(4)), 0);
        }

        #[test]
        fn adopt_index_skips_pinned_tabs() {
            let mut pinned = tab(Some("a"), Some(0));
            pinned.pinned = true;
            let tabs = [pinned, tab(Some("a"), Some(1))];
            assert_eq!(adopt_index(&tabs, Some("a"), Some(0)), 1);
        }

        #[test]
        fn the_window_with_most_tabs_adopts_and_the_frontmost_wins_ties() {
            assert_eq!(pick_adopter(&[(1, 2), (2, 5), (3, 5)]), Some(2));
            assert_eq!(pick_adopter(&[(7, 0)]), Some(7));
            assert_eq!(pick_adopter::<u32>(&[]), None);
        }
    }
}

/// Claude Code's own record of a session waiting on the user (`~/.claude/sessions/<pid>.json`,
/// `status: "waiting"`), for dialogs that fire no hook. Pure logic only; reading the files and
/// tmux is in `tmux_agents::claude_waits_by_terminal`, the status change in
/// `CLIAgentSessionsModel::set_external_wait`.
///
/// How this can go wrong, and the test that pins each case:
/// 1. A waiting session is not recognised: `a_waiting_file_is_read_with_its_pane`.
/// 2. A file left by a dead Claude Code keeps a tab on "needs you":
///    `files_of_dead_processes_and_other_states_are_ignored`.
/// 3. The wait lands on the wrong tab: `waits_map_through_their_pane_to_its_session`.
/// Sticking after the dialog is answered and a sound per poll are covered in the
/// `cli_agent_sessions` tests.
#[cfg(not(target_family = "wasm"))]
pub(in crate::workspace::view) mod claude_waits {
    use std::collections::HashMap;

    use serde::Deserialize;

    /// A Claude Code waiting on the user in a tmux pane.
    #[derive(Clone, Debug, PartialEq)]
    pub(in crate::workspace::view) struct Wait {
        /// tmux pane id, e.g. `%49`.
        pub pane: String,
        pub pid: u32,
        /// What it waits for ("permission prompt"), shown as the row's reason.
        pub label: String,
        /// When the wait started (Unix ms).
        pub updated_ms: u64,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct SessionFile {
        pid: u32,
        status: Option<String>,
        waiting_for: Option<String>,
        status_updated_at: Option<u64>,
        tmux: Option<String>,
    }

    /// The wait a session file records, if its status is "waiting" and it names a tmux pane
    /// (`"<name>:@<window>.%<pane>"`).
    pub(in crate::workspace::view) fn parse_session_file(json: &str) -> Option<Wait> {
        let file: SessionFile = serde_json::from_str(json).ok()?;
        if file.status.as_deref() != Some("waiting") {
            return None;
        }
        let tmux = file.tmux?;
        let pane = tmux.rsplit('.').next().filter(|pane| {
            pane.strip_prefix('%').is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
            })
        })?;
        Some(Wait {
            pane: pane.to_owned(),
            pid: file.pid,
            label: file
                .waiting_for
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| "Waiting for you".to_owned()),
            updated_ms: file.status_updated_at.unwrap_or_default(),
        })
    }

    /// `tmux list-panes -a -F '#{pane_id} #{session_id}'` as pane to session.
    pub(in crate::workspace::view) fn parse_pane_sessions(output: &str) -> HashMap<String, String> {
        output
            .lines()
            .filter_map(|line| {
                let (pane, session) = line.split_once(' ')?;
                Some((pane.to_owned(), session.trim().to_owned()))
            })
            .collect()
    }

    /// The newest wait per tmux session, from live processes only.
    pub(in crate::workspace::view) fn waits_by_session(
        waits: Vec<Wait>,
        pane_sessions: &HashMap<String, String>,
        alive: impl Fn(u32) -> bool,
    ) -> HashMap<String, Wait> {
        let mut by_session: HashMap<String, Wait> = HashMap::new();
        for wait in waits {
            if !alive(wait.pid) {
                continue;
            }
            let Some(session) = pane_sessions.get(&wait.pane) else {
                continue;
            };
            if by_session
                .get(session)
                .is_none_or(|known| known.updated_ms < wait.updated_ms)
            {
                by_session.insert(session.clone(), wait);
            }
        }
        by_session
    }

    /// Claude Code's open dialog read from Warp's own terminal grid (no tmux needed), for the
    /// notifications in `notify`.
    pub(in crate::workspace::view) mod dialog {
        pub(in crate::workspace::view) const DIALOG_CHANGED: &str =
            "The question changed in the terminal; nothing was sent";

        /// A ❯ list Claude Code draws (warp-mobile bridge's `Dialog`).
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub(in crate::workspace::view) struct ScreenDialog {
            pub title: String,
            pub body: Vec<String>,
            pub options: Vec<String>,
            /// Deeper lines under each option: a wrapped label or a hint.
            pub details: Vec<String>,
            /// The option the ❯ points at.
            pub selected: usize,
        }

        impl ScreenDialog {
            /// Everything the notification showed but the ❯: a moved cursor is no new dialog, but
            /// a command that changed only further down is.
            pub(in crate::workspace::view) fn same_as(&self, other: &ScreenDialog) -> bool {
                self.title == other.title
                    && self.body == other.body
                    && self.options == other.options
                    && self.details == other.details
            }
        }

        fn run_of(line: &str, ch: char, min: usize) -> bool {
            let line = line.trim();
            line.chars().count() >= min && line.chars().all(|c| c == ch)
        }

        fn is_rule(line: &str) -> bool {
            run_of(line, '─', 20)
        }

        fn is_dashed(line: &str) -> bool {
            run_of(line, '╌', 20)
        }

        fn is_panel_top(line: &str) -> bool {
            line.trim_start().chars().take_while(|&c| c == '▔').count() >= 10
        }

        fn indent(line: &str) -> usize {
            line.chars().take_while(|c| c.is_whitespace()).count()
        }

        /// The text column after `  ❯ ` on a dialog's cursor line; the input box's ❯ sits at
        /// column 0 and never counts.
        fn cursor_column(line: &str) -> Option<usize> {
            let lead = indent(line);
            let rest = line.trim_start().strip_prefix('❯')?;
            let gap = rest.chars().take_while(|c| c.is_whitespace()).count();
            let after = rest.trim_start();
            (lead > 0 && gap > 0 && !after.is_empty()).then_some(lead + 1 + gap)
        }

        /// The lowest ❯ list on `screen`, read as warp-mobile's bridge reads it (`dialogOnScreen`).
        pub(in crate::workspace::view) fn dialog_on_screen(screen: &str) -> Option<ScreenDialog> {
            let lines: Vec<&str> = screen.split('\n').map(str::trim_end).collect();
            let c = lines.iter().rposition(|line| cursor_column(line).is_some())?;
            let col = cursor_column(lines[c])?;
            let edge = |line: &str| {
                line.trim().is_empty()
                    || is_rule(line)
                    || is_dashed(line)
                    || line.trim_start().starts_with(['│', '╭', '╰'])
                    || indent(line) < col
            };
            let mut start = c;
            while start > 0 && !edge(lines[start - 1]) {
                start -= 1;
            }
            while start < c && indent(lines[start]) != col {
                start += 1;
            }
            let mut end = c + 1;
            while end < lines.len() && !edge(lines[end]) {
                end += 1;
            }
            let mut options: Vec<String> = Vec::new();
            let mut details: Vec<String> = Vec::new();
            let mut selected = 0;
            for (i, line) in lines.iter().enumerate().take(end).skip(start) {
                if i == c {
                    selected = options.len();
                    let label = line.trim_start().trim_start_matches('❯').trim_start();
                    options.push(label.to_owned());
                    details.push(String::new());
                } else if indent(line) == col {
                    options.push(line.trim().to_owned());
                    details.push(String::new());
                } else if let Some(last) = details.last_mut() {
                    *last = format!("{last} {}", line.trim()).trim().to_owned();
                }
            }
            let numbered = options
                .iter()
                .enumerate()
                .all(|(i, option)| option.starts_with(&format!("{}. ", i + 1)));
            let options = options
                .into_iter()
                .map(|option| {
                    if numbered {
                        let (_, label) = option.split_once(". ").unwrap_or_default();
                        label.trim_start().to_owned()
                    } else {
                        option
                    }
                })
                .collect();
            let mut top = lines[..start]
                .iter()
                .rposition(|line| is_rule(line) || is_panel_top(line));
            // Stacked sections (plan approval): up over rules until transcript lines sit between.
            while let Some(at) = top.filter(|&at| at > 0) {
                let Some(prev) = lines[..at].iter().rposition(|line| is_rule(line)) else {
                    break;
                };
                if lines[prev + 1..at].iter().any(|line| {
                    line.trim_start().starts_with(['⏺', '⎿', '▔', '▁']) || line.starts_with('❯')
                }) {
                    break;
                }
                top = Some(prev);
            }
            if top.is_none() {
                top = lines[..start].iter().rposition(|line| is_dashed(line));
            }
            let from = top.map_or(start.saturating_sub(12), |at| at + 1);
            let text: Vec<String> = lines[from..start]
                .iter()
                .filter(|line| !is_dashed(line) && !is_rule(line) && !is_panel_top(line))
                .map(|line| {
                    let line = line.trim();
                    match line.strip_prefix('│') {
                        Some(rest) => rest.strip_prefix(char::is_whitespace).unwrap_or(rest),
                        None => line,
                    }
                    .to_owned()
                })
                .filter(|line| !line.is_empty())
                .collect();
            Some(ScreenDialog {
                title: text.first().cloned().unwrap_or_default(),
                body: text.iter().skip(1).cloned().collect(),
                options,
                details,
                selected,
            })
        }

        /// What a notification shows for a waiting Claude.
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub(in crate::workspace::view) enum Card {
            /// A tool permission ("Do you want to …?").
            Approval(ScreenDialog),
            /// Any other ❯ list: a question, plan approval, the trust dialog, a picker.
            Dialog(ScreenDialog),
            /// Claude waits but the screen shows no dialog to read: the wait's own message.
            Waiting(String),
        }

        /// The card for what the screen shows; `None` without a ❯ list.
        pub(in crate::workspace::view) fn card_on_screen(screen: &str) -> Option<Card> {
            let dialog = dialog_on_screen(screen)?;
            let question = dialog.body.last().unwrap_or(&dialog.title);
            // An AskUserQuestion dialog can ask "Do you want to …?" too; it always offers
            // "Type something.", which a permission never does.
            let asks = dialog
                .options
                .iter()
                .any(|option| option.trim_start_matches("[ ]").trim().starts_with("Type something"));
            if !asks && question.starts_with("Do you want to ") && question.ends_with('?') {
                return Some(Card::Approval(dialog));
            }
            Some(Card::Dialog(dialog))
        }

        /// Whether `command` (a terminal's running command) is a terminal multiplexer, which can
        /// draw one pane and send the keys it gets to another.
        pub(in crate::workspace::view) fn runs_multiplexer(command: &str) -> bool {
            command
                .split_whitespace()
                .find(|word| !word.contains('=') && !matches!(*word, "exec" | "command" | "env"))
                .and_then(|program| program.rsplit('/').next())
                .is_some_and(|program| {
                    matches!(program, "tmux" | "screen" | "zellij" | "byobu" | "dtach" | "abduco")
                })
        }

        /// What a permission names from the screen alone: the first line under the dialog's
        /// title, never Claude's "Tip: …" line.
        pub(in crate::workspace::view) fn approval_subject(dialog: &ScreenDialog) -> &str {
            let question = dialog.body.last();
            dialog
                .body
                .iter()
                .find(|line| Some(*line) != question && !line.starts_with("Tip:"))
                .unwrap_or(&dialog.title)
        }

        #[cfg(test)]
        pub(in crate::workspace::view) mod tests {
            use super::*;

            pub(in crate::workspace::view) const PERMISSION: &str = "⏺ Write(/tmp/x)\n\n────────────────────────────────────────\n Create file\n /tmp/x\n╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n  1 hello\n╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n Do you want to create x?\n ❯ 1. Yes\n   2. Yes, allow all edits during this session (shift+tab)\n   3. No, and tell Claude what to do differently (esc)\n\n Esc to cancel · Tab to amend\n";

            /// `PERMISSION` with the ❯ on option `at` (0-based).
            pub(in crate::workspace::view) fn permission_at(at: usize) -> String {
                let labels = [
                    "1. Yes",
                    "2. Yes, allow all edits during this session (shift+tab)",
                    "3. No, and tell Claude what to do differently (esc)",
                ];
                let head = PERMISSION.split(" ❯ 1. Yes").next().unwrap();
                let mut screen = head.to_owned();
                for (i, label) in labels.iter().enumerate() {
                    screen.push_str(if i == at { " ❯ " } else { "   " });
                    screen.push_str(label);
                    screen.push('\n');
                }
                screen.push_str("\n Esc to cancel · Tab to amend\n");
                screen
            }

            pub(in crate::workspace::view) const OTHER: &str = "────────────────────────────────────────\n Bash command\n   rm -rf build\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n";

            const ASK: &str = "☐ Database\n\nDo you want to use Postgres?\n\n ❯ 1. Yes\n   2. No\n   3. Type something.\n   4. Chat about this\n";

            /// The input box below an answered dialog: no ❯ list.
            const PLAIN: &str = "⏺ Done.\n\n────────────────────────────────────────\n❯ \n────────────────────────────────────────\n  ? for shortcuts\n";

            /// A real Claude Code 2.1.304 Bash permission (warp-mobile bridge fixture): the
            /// command between dashed rules, a "Tip:" and a description above it.
            fn bash_prompt() -> String {
                let (rule, dashed) = ("─".repeat(120), "╌".repeat(120));
                [
                    "❯ Run this exact bash command and nothing else: date > /tmp/wm-x/bash.txt",
                    "",
                    "  Writing current date to file",
                    "  ⎿  $ date > /tmp/wm-x/bash.txt",
                    "",
                    &rule,
                    " Bash command",
                    " Tip: auto mode handles these prompts for you — choose \"switch to auto mode\" below",
                    " Write current date to file",
                    &dashed,
                    " date > /tmp/wm-x/bash.txt",
                    &dashed,
                    " Do you want to proceed?",
                    " ❯ 1. Yes",
                    "   2. Yes, and always allow access to /tmp/wm-x from this project",
                    "   3. Yes, and switch to auto mode · auto mode handles these prompts for you",
                    "   4. No",
                    "",
                    " Esc to cancel · Tab to amend",
                ]
                .join("\n")
            }

            #[test]
            fn a_permission_dialog_reads_as_the_phone_reads_it() {
                let dialog = dialog_on_screen(PERMISSION).unwrap();
                assert_eq!(dialog.title, "Create file");
                assert_eq!(dialog.body.last().map(String::as_str), Some("Do you want to create x?"));
                assert_eq!(
                    dialog.options,
                    [
                        "Yes",
                        "Yes, allow all edits during this session (shift+tab)",
                        "No, and tell Claude what to do differently (esc)"
                    ]
                );
                assert_eq!(dialog.selected, 0);
                assert_eq!(dialog_on_screen(&permission_at(2)).unwrap().selected, 2);
            }

            #[test]
            fn a_moved_cursor_is_the_same_dialog_another_command_is_not() {
                let shown = dialog_on_screen(PERMISSION).unwrap();
                assert!(dialog_on_screen(&permission_at(1)).unwrap().same_as(&shown));
                let other = dialog_on_screen(&PERMISSION.replace("1 hello", "1 bye")).unwrap();
                assert!(!other.same_as(&shown));
            }

            #[test]
            fn only_a_tool_permission_gets_allow_and_deny() {
                assert!(matches!(card_on_screen(PERMISSION), Some(Card::Approval(_))));
                assert!(matches!(card_on_screen(OTHER), Some(Card::Approval(_))));
                assert!(matches!(card_on_screen(&bash_prompt()), Some(Card::Approval(_))));
                // A question that happens to ask "Do you want to …?" is no permission.
                assert!(matches!(card_on_screen(ASK), Some(Card::Dialog(_))));
                assert_eq!(card_on_screen(PLAIN), None);
            }

            #[test]
            fn a_multiplexer_is_never_typed_through() {
                assert!(runs_multiplexer("tmux attach -t 3"));
                assert!(runs_multiplexer("TERM=xterm /opt/homebrew/bin/tmux new"));
                assert!(runs_multiplexer("exec screen -r"));
                assert!(!runs_multiplexer("claude --permission-mode default"));
                assert!(!runs_multiplexer("HOME=/Users/x claude 'run tmux ls'"));
                assert!(!runs_multiplexer(""));
            }

            #[test]
            fn a_permission_names_what_claude_wants_never_the_tip() {
                assert_eq!(approval_subject(&dialog_on_screen(PERMISSION).unwrap()), "/tmp/x");
                assert_eq!(approval_subject(&dialog_on_screen(OTHER).unwrap()), "rm -rf build");
                let bash = dialog_on_screen(&bash_prompt()).unwrap();
                assert_eq!(approval_subject(&bash), "Write current date to file");
            }
        }
    }

    /// Desktop notifications for Claude Code dialogs, with Allow, Deny and Open chat. The trigger
    /// is the one the sidebar paints "needs you" from: a Claude Code session in a visible terminal
    /// that is blocked. Pure logic only; reading the grid, posting and pressing keys is in
    /// `Workspace::sync_claude_notices` and `Workspace::claude_notice_clicked`.
    ///
    /// How this can go wrong, and the test that pins each case:
    /// 1. Stale dialog: the notification showed one dialog, the screen now shows another (or the
    ///    same title with another command), or none. An answer must press nothing:
    ///    `an_action_answers_only_the_dialog_its_notification_showed`,
    ///    `answer::tests::an_answer_presses_nothing_once_the_dialog_changed`,
    ///    `answer::tests::an_answer_stops_when_the_dialog_changes_mid_selection`.
    /// 2. Wrong pane: keys may only go to the terminal whose screen showed the dialog, never to a
    ///    split next to it, and never through a full-screen program (tmux, screen) that can route
    ///    them to a pane other than the one it draws: entries are kept per terminal
    ///    (`each_terminal_gets_its_own_notification`), an answer re-reads that very terminal
    ///    before every key, and a dialog behind a multiplexer only opens the chat
    ///    (`a_dialog_behind_a_full_screen_program_only_opens_the_chat`,
    ///    `dialog::tests::a_multiplexer_is_never_typed_through`).
    /// 3. App in the background: a dialog notifies only while its terminal is out of view, one
    ///    first seen in view never notifies, and one that comes into view is taken back:
    ///    `the_terminal_in_view_gets_no_notification`, `only_the_front_window_s_active_tab_is_in_view`.
    ///    Allow and Deny answer without bringing the app forward (notifications.m).
    /// 4. Answered in the terminal: once Claude stops waiting or the dialog leaves the screen the
    ///    notification is taken back, and a late click on it refuses:
    ///    `an_answered_dialog_takes_its_notification_back`, `one_dialog_notifies_once`,
    ///    `answer::tests::a_claude_that_stopped_waiting_gets_no_key`.
    /// 5. Keys that do not land are reported, not repeated:
    ///    `answer::tests::a_selection_that_does_not_move_is_reported`,
    ///    `answer::tests::enter_that_leaves_the_dialog_open_is_reported`.
    /// 6. A hook that reports the wait before Claude drew its dialog must not post a generic
    ///    notification right before the real one: `a_wait_without_a_dialog_notifies_one_read_later`.
    pub(in crate::workspace::view) mod notify {
        use std::collections::{HashMap, HashSet};
        use std::hash::Hash;

        use super::dialog::{self, Card};

        /// Registered at startup with Allow, Deny and Open chat (warpui's notifications.m).
        pub(in crate::workspace::view) const APPROVAL: &str = "HARP_APPROVAL";
        /// Open chat only.
        pub(in crate::workspace::view) const OPEN_ONLY: &str = "HARP_QUESTION";
        const ALLOW: &str = "approve-0";
        const DENY: &str = "approve-deny";

        /// A dialog notified, or seen in view and so never notified, per terminal.
        #[derive(Clone, Debug)]
        pub(in crate::workspace::view) struct Entry<X> {
            /// The notification's identifier, also in its data.
            pub id: String,
            /// The dialog as the notification showed it: what an answer must still find.
            pub card: Card,
            /// Whether the notification is up.
            pub posted: bool,
            /// A wait without a dialog, read once: it posts on the next read if it still has none.
            pub held: bool,
            /// Where to answer it.
            pub target: X,
        }

        /// What a waiting terminal's screen shows now.
        pub(in crate::workspace::view) struct Read<X> {
            pub card: Card,
            pub target: X,
        }

        #[derive(Debug, PartialEq, Eq)]
        pub(in crate::workspace::view) enum Change<T> {
            Post(T),
            TakeBack(String),
        }

        /// What a click on a notification does.
        #[derive(Debug, PartialEq, Eq)]
        pub(in crate::workspace::view) enum Act {
            /// Pick this option of the dialog the notification showed.
            Pick(usize),
            Open,
            /// Allow or Deny for a dialog that is gone: nothing is sent, the reason shows.
            Refuse(&'static str),
        }

        fn same(a: &Card, b: &Card) -> bool {
            match (a, b) {
                (Card::Approval(a), Card::Approval(b)) | (Card::Dialog(a), Card::Dialog(b)) => {
                    a.same_as(b)
                }
                (Card::Waiting(a), Card::Waiting(b)) => a == b,
                _ => false,
            }
        }

        /// The tab the user looks at: the active tab of the front window while the app is in
        /// front.
        pub(in crate::workspace::view) fn in_view<T>(
            app_active: bool,
            window_active: bool,
            active_tab: Option<T>,
        ) -> Option<T> {
            (app_active && window_active).then_some(active_tab).flatten()
        }

        /// Brings `entries` up to `waiting` (each terminal whose Claude waits, and what its screen
        /// shows) and says which notifications to send and which to take back. A terminal missing
        /// from `waiting` no longer waits or closed. A dialog first seen while its terminal is in
        /// view (`in_view`) is kept unsent: the user saw it, so it never notifies. So is a wait
        /// without a dialog in a terminal whose dialog was just answered from a notification
        /// (`answered`): Claude fires no hook on a denial (nor before a long allowed tool ends),
        /// so its need, and the denied command's text, outlive the dialog.
        pub(in crate::workspace::view) fn update<T: Copy + Eq + Hash, X>(
            entries: &mut HashMap<T, Entry<X>>,
            waiting: Vec<(T, Read<X>)>,
            in_view: &HashSet<T>,
            answered: &HashSet<T>,
            mut new_id: impl FnMut() -> String,
        ) -> Vec<Change<T>> {
            let mut changes = Vec::new();
            // New dialogs go out in `waiting`'s order, never a HashMap's: ids and send order stay
            // the same from run to run.
            let order: Vec<T> = waiting.iter().map(|(terminal, _)| *terminal).collect();
            let mut now: HashMap<T, Read<X>> = waiting.into_iter().collect();
            entries.retain(|terminal, entry| {
                let keep = now
                    .get(terminal)
                    .is_some_and(|read| same(&entry.card, &read.card));
                if !keep && entry.posted {
                    changes.push(Change::TakeBack(entry.id.clone()));
                }
                keep
            });
            for terminal in order {
                let Some(read) = now.remove(&terminal) else {
                    continue;
                };
                let seen = in_view.contains(&terminal)
                    || (answered.contains(&terminal) && matches!(read.card, Card::Waiting(_)));
                if let Some(entry) = entries.get_mut(&terminal) {
                    if seen {
                        entry.held = false;
                        if entry.posted {
                            entry.posted = false;
                            changes.push(Change::TakeBack(entry.id.clone()));
                        }
                    } else if entry.held {
                        entry.held = false;
                        entry.posted = true;
                        changes.push(Change::Post(terminal));
                    }
                    continue;
                }
                let held = !seen && matches!(read.card, Card::Waiting(_));
                let post = !seen && !held;
                entries.insert(
                    terminal,
                    Entry {
                        id: new_id(),
                        card: read.card,
                        posted: post,
                        held,
                        target: read.target,
                    },
                );
                if post {
                    changes.push(Change::Post(terminal));
                }
            }
            changes
        }

        /// The entry a click on notification `id` of `terminal` is for; `None` once its dialog is
        /// gone or another took its place. Its notification counts as down: the click took it.
        pub(in crate::workspace::view) fn clicked<'a, T: Eq + Hash, X>(
            entries: &'a mut HashMap<T, Entry<X>>,
            terminal: &T,
            id: &str,
        ) -> Option<&'a Entry<X>> {
            let entry = entries.get_mut(terminal).filter(|entry| entry.id == id)?;
            entry.posted = false;
            Some(entry)
        }

        fn is_permission(card: &Card, answerable: bool) -> bool {
            answerable && matches!(card, Card::Approval(dialog) if dialog.options.len() >= 2)
        }

        /// What `action` does on `card` (the dialog its notification showed, `None` when gone):
        /// Allow picks the first option and Deny the last, only on a permission the app can answer
        /// (`answerable`); Open chat or a click on the notification itself opens the chat.
        pub(in crate::workspace::view) fn act(
            action: Option<&str>,
            card: Option<&Card>,
            answerable: bool,
        ) -> Act {
            let answer = matches!(action, Some(ALLOW | DENY));
            match card {
                Some(card @ Card::Approval(dialog)) if answer && is_permission(card, answerable) => {
                    Act::Pick(if action == Some(ALLOW) {
                        0
                    } else {
                        dialog.options.len() - 1
                    })
                }
                _ if answer => Act::Refuse(dialog::DIALOG_CHANGED),
                _ => Act::Open,
            }
        }

        /// The category (its buttons) and body of a dialog's notification.
        pub(in crate::workspace::view) fn content(
            card: &Card,
            answerable: bool,
        ) -> (&'static str, String) {
            match card {
                Card::Approval(dialog) => (
                    if is_permission(card, answerable) {
                        APPROVAL
                    } else {
                        OPEN_ONLY
                    },
                    format!("Needs you: Allow {}?", cut(dialog::approval_subject(dialog), 200)),
                ),
                Card::Dialog(dialog) => {
                    (OPEN_ONLY, cut(dialog.body.last().unwrap_or(&dialog.title), 300))
                }
                Card::Waiting(message) => (OPEN_ONLY, cut(message, 300)),
            }
        }

        fn cut(text: &str, max: usize) -> String {
            if text.chars().count() <= max {
                return text.to_owned();
            }
            let kept: String = text.chars().take(max - 1).collect();
            format!("{kept}…")
        }

        #[cfg(test)]
        mod tests {
            use super::super::dialog::dialog_on_screen;
            use super::super::dialog::tests::{OTHER, PERMISSION, permission_at};
            use super::*;

            fn permission(screen: &str) -> Card {
                Card::Approval(dialog_on_screen(screen).unwrap())
            }

            fn ids() -> impl FnMut() -> String {
                let mut n = 0;
                move || {
                    n += 1;
                    format!("n{n}")
                }
            }

            fn read(card: Card) -> Read<()> {
                Read { card, target: () }
            }

            fn none() -> HashSet<u32> {
                HashSet::new()
            }

            #[test]
            fn an_action_answers_only_the_dialog_its_notification_showed() {
                let card = permission(PERMISSION);
                assert_eq!(act(Some(ALLOW), Some(&card), true), Act::Pick(0));
                assert_eq!(act(Some(DENY), Some(&card), true), Act::Pick(2));
                // The dialog is gone or was replaced: nothing is pressed.
                assert_eq!(act(Some(ALLOW), None, true), Act::Refuse(dialog::DIALOG_CHANGED));
                assert_eq!(act(Some(DENY), None, true), Act::Refuse(dialog::DIALOG_CHANGED));
                assert_eq!(act(Some("open"), None, true), Act::Open);
                assert_eq!(act(None, Some(&card), true), Act::Open);
            }

            #[test]
            fn allow_and_deny_only_on_a_permission() {
                let other = Card::Dialog(dialog_on_screen(OTHER).unwrap());
                assert_eq!(
                    act(Some(ALLOW), Some(&other), true),
                    Act::Refuse(dialog::DIALOG_CHANGED)
                );
                assert_eq!(content(&other, true).0, OPEN_ONLY);
                let waiting = Card::Waiting("Claude needs your input".to_owned());
                assert_eq!(
                    content(&waiting, true),
                    (OPEN_ONLY, "Claude needs your input".to_owned())
                );
                let (category, body) = content(&permission(OTHER), true);
                assert_eq!(
                    (category, body.as_str()),
                    (APPROVAL, "Needs you: Allow rm -rf build?")
                );
            }

            #[test]
            fn a_dialog_behind_a_full_screen_program_only_opens_the_chat() {
                let card = permission(PERMISSION);
                assert_eq!(content(&card, false).0, OPEN_ONLY);
                assert_eq!(
                    act(Some(ALLOW), Some(&card), false),
                    Act::Refuse(dialog::DIALOG_CHANGED)
                );
            }

            #[test]
            fn each_terminal_gets_its_own_notification() {
                let mut entries = HashMap::new();
                let changes = update(
                    &mut entries,
                    vec![(1, read(permission(PERMISSION))), (2, read(permission(OTHER)))],
                    &none(),
                    &none(),
                    ids(),
                );
                assert_eq!(changes, [Change::Post(1), Change::Post(2)]);
                // A click on terminal 1's notification never finds terminal 2's dialog.
                assert!(clicked(&mut entries, &2, "n1").is_none());
                assert!(clicked(&mut entries, &1, "n1").is_some());
            }

            #[test]
            fn the_terminal_in_view_gets_no_notification() {
                let mut entries = HashMap::new();
                let seen = HashSet::from([1]);
                let first = update(&mut entries, vec![(1, read(permission(PERMISSION)))], &seen, &none(), ids());
                assert_eq!(first, []);
                // Looked away: the dialog was seen, so it still does not notify.
                let away = update(&mut entries, vec![(1, read(permission(PERMISSION)))], &none(), &none(), ids());
                assert_eq!(away, []);
                // Notified, then its terminal comes into view: taken back.
                let mut entries = HashMap::new();
                update(&mut entries, vec![(2, read(permission(OTHER)))], &none(), &none(), ids());
                let seen = HashSet::from([2]);
                let back = update(&mut entries, vec![(2, read(permission(OTHER)))], &seen, &none(), ids());
                assert_eq!(back, [Change::TakeBack("n1".to_owned())]);
            }

            #[test]
            fn only_the_front_window_s_active_tab_is_in_view() {
                assert_eq!(in_view(true, true, Some(7)), Some(7));
                assert_eq!(in_view(false, true, Some(7)), None);
                assert_eq!(in_view(true, false, Some(7)), None);
                assert_eq!(in_view::<u32>(true, true, None), None);
            }

            #[test]
            fn an_answered_dialog_takes_its_notification_back() {
                let mut entries = HashMap::new();
                update(&mut entries, vec![(1, read(permission(PERMISSION)))], &none(), &none(), ids());
                // Answered in the terminal: Claude no longer waits.
                let changes = update(&mut entries, vec![], &none(), &none(), ids());
                assert_eq!(changes, [Change::TakeBack("n1".to_owned())]);
                assert!(entries.is_empty());
                // A late click on it finds nothing, so Allow refuses.
                assert!(clicked(&mut entries, &1, "n1").is_none());
            }

            #[test]
            fn one_dialog_notifies_once() {
                let mut entries = HashMap::new();
                let mut new_id = ids();
                let first = update(
                    &mut entries,
                    vec![(1, read(permission(PERMISSION)))],
                    &none(),
                    &none(),
                    &mut new_id,
                );
                assert_eq!(first, [Change::Post(1)]);
                // The user moved the ❯: the same dialog, no second notification.
                let moved = read(permission(&permission_at(1)));
                assert_eq!(update(&mut entries, vec![(1, moved)], &none(), &none(), &mut new_id), []);
                // Another dialog in its place: the old one goes, the new one posts.
                let next = update(&mut entries, vec![(1, read(permission(OTHER)))], &none(), &none(), &mut new_id);
                assert_eq!(next, [Change::TakeBack("n1".to_owned()), Change::Post(1)]);
                assert_eq!(entries[&1].id, "n2");
            }

            #[test]
            fn a_wait_without_a_dialog_notifies_one_read_later() {
                let waiting = || read(Card::Waiting("Claude needs your input".to_owned()));
                let mut entries = HashMap::new();
                let mut new_id = ids();
                assert_eq!(update(&mut entries, vec![(1, waiting())], &none(), &none(), &mut new_id), []);
                // Its dialog appears on the next read: only the dialog posts.
                let dialog = read(permission(PERMISSION));
                let changes = update(&mut entries, vec![(1, dialog)], &none(), &none(), &mut new_id);
                assert_eq!(changes, [Change::Post(1)]);
                // Still no dialog on the second read: the wait itself posts, once.
                let mut entries = HashMap::new();
                update(&mut entries, vec![(1, waiting())], &none(), &none(), &mut new_id);
                let second = update(&mut entries, vec![(1, waiting())], &none(), &none(), &mut new_id);
                assert_eq!(second, [Change::Post(1)]);
                assert_eq!(update(&mut entries, vec![(1, waiting())], &none(), &none(), &mut new_id), []);
            }

            #[test]
            fn a_wait_left_by_an_answer_from_a_notification_never_notifies() {
                let stale = || read(Card::Waiting("Wants to run Bash: mkdir x".to_owned()));
                let answered = HashSet::from([1]);
                let mut entries = HashMap::new();
                let mut new_id = ids();
                assert_eq!(update(&mut entries, vec![(1, stale())], &none(), &answered, &mut new_id), []);
                // Only the read right after the answer knows it; the wait stays quiet after it.
                assert_eq!(update(&mut entries, vec![(1, stale())], &none(), &none(), &mut new_id), []);
                assert_eq!(update(&mut entries, vec![(1, stale())], &none(), &none(), &mut new_id), []);
                assert!(!entries[&1].posted);
            }

            #[test]
            fn a_new_dialog_right_after_an_answer_still_notifies() {
                let answered = HashSet::from([1]);
                let mut entries = HashMap::new();
                let changes =
                    update(&mut entries, vec![(1, read(permission(OTHER)))], &none(), &answered, ids());
                assert_eq!(changes, [Change::Post(1)]);
            }
        }
    }

    /// Allow or Deny as timed key presses, one step per main-thread tick: the pty is only written
    /// there, and every key waits for Claude to redraw before the screen is read again.
    pub(in crate::workspace::view) mod answer {
        use super::dialog::{DIALOG_CHANGED, ScreenDialog};

        /// The pause between keys and between the checks after Enter.
        pub(in crate::workspace::view) const KEY_STEP: std::time::Duration =
            std::time::Duration::from_millis(200);
        const CHECKS_AFTER_ENTER: usize = 10;

        pub(in crate::workspace::view) const NOT_MOVED: &str =
            "The selection did not move; check the terminal";
        pub(in crate::workspace::view) const STILL_OPEN: &str =
            "Enter was sent but the question is still open; check the terminal";
        pub(in crate::workspace::view) const TERMINAL_GONE: &str =
            "The terminal of that question is gone; nothing was sent";

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(in crate::workspace::view) enum Key {
            Up,
            Down,
            Enter,
        }

        #[derive(Debug, PartialEq, Eq)]
        pub(in crate::workspace::view) enum Step {
            Press(Key),
            /// Read again after the next pause.
            Wait,
            Done,
            Refuse(&'static str),
        }

        /// Picking option `index` of `shown`, the dialog the notification showed.
        #[derive(Clone, Debug)]
        pub(in crate::workspace::view) struct Answer {
            shown: ScreenDialog,
            index: usize,
            moves: usize,
            entered: bool,
            checks: usize,
        }

        impl Answer {
            pub(in crate::workspace::view) fn new(shown: ScreenDialog, index: usize) -> Self {
                Self {
                    shown,
                    index,
                    moves: 0,
                    entered: false,
                    checks: 0,
                }
            }

            /// The next step for what the terminal shows now (`now`: its dialog, `None` without
            /// one) while its Claude still waits (`waits`). Before Enter, anything but the very
            /// dialog the notification showed stops the answer without a key.
            pub(in crate::workspace::view) fn next(
                &mut self,
                now: Option<&ScreenDialog>,
                waits: bool,
            ) -> Step {
                if self.entered {
                    if !now.is_some_and(|now| now.same_as(&self.shown)) {
                        return Step::Done;
                    }
                    self.checks += 1;
                    return if self.checks >= CHECKS_AFTER_ENTER {
                        Step::Refuse(STILL_OPEN)
                    } else {
                        Step::Wait
                    };
                }
                let Some(now) = now.filter(|now| waits && now.same_as(&self.shown)) else {
                    return Step::Refuse(DIALOG_CHANGED);
                };
                if now.selected == self.index {
                    // ponytail: the read and the Enter are one tick apart, but a key the user typed
                    // that is still in the pty can land first. Closing it needs Claude Code to take
                    // an answer by dialog id instead of by key.
                    self.entered = true;
                    return Step::Press(Key::Enter);
                }
                if self.moves > self.shown.options.len() {
                    return Step::Refuse(NOT_MOVED);
                }
                self.moves += 1;
                Step::Press(if now.selected < self.index {
                    Key::Down
                } else {
                    Key::Up
                })
            }
        }

        #[cfg(test)]
        mod tests {
            use super::super::dialog::dialog_on_screen;
            use super::super::dialog::tests::{PERMISSION, permission_at};
            use super::*;

            fn at(selected: usize) -> ScreenDialog {
                dialog_on_screen(&permission_at(selected)).unwrap()
            }

            #[test]
            fn allow_on_the_selected_option_is_one_enter_then_done() {
                let mut answer = Answer::new(at(0), 0);
                assert_eq!(answer.next(Some(&at(0)), true), Step::Press(Key::Enter));
                // The dialog left the screen: answered.
                assert_eq!(answer.next(None, false), Step::Done);
            }

            #[test]
            fn deny_moves_down_to_the_last_option_then_enters() {
                let mut answer = Answer::new(at(0), 2);
                assert_eq!(answer.next(Some(&at(0)), true), Step::Press(Key::Down));
                assert_eq!(answer.next(Some(&at(1)), true), Step::Press(Key::Down));
                assert_eq!(answer.next(Some(&at(2)), true), Step::Press(Key::Enter));
                assert_eq!(answer.next(Some(&at(2)), false), Step::Wait);
                assert_eq!(answer.next(None, false), Step::Done);
            }

            #[test]
            fn a_cursor_the_user_moved_is_walked_back() {
                // The user left the ❯ on the last option; Allow goes up to the first.
                let mut answer = Answer::new(at(0), 0);
                assert_eq!(answer.next(Some(&at(2)), true), Step::Press(Key::Up));
                assert_eq!(answer.next(Some(&at(1)), true), Step::Press(Key::Up));
                assert_eq!(answer.next(Some(&at(0)), true), Step::Press(Key::Enter));
            }

            #[test]
            fn an_answer_presses_nothing_once_the_dialog_changed() {
                let other = dialog_on_screen(&PERMISSION.replace("1 hello", "1 bye")).unwrap();
                let mut answer = Answer::new(at(0), 0);
                assert_eq!(answer.next(Some(&other), true), Step::Refuse(DIALOG_CHANGED));
                let mut answer = Answer::new(at(0), 0);
                assert_eq!(answer.next(None, true), Step::Refuse(DIALOG_CHANGED));
            }

            #[test]
            fn an_answer_stops_when_the_dialog_changes_mid_selection() {
                let other = dialog_on_screen(&PERMISSION.replace("1 hello", "1 bye")).unwrap();
                let mut answer = Answer::new(at(0), 2);
                assert_eq!(answer.next(Some(&at(0)), true), Step::Press(Key::Down));
                assert_eq!(answer.next(Some(&other), true), Step::Refuse(DIALOG_CHANGED));
            }

            #[test]
            fn a_claude_that_stopped_waiting_gets_no_key() {
                let mut answer = Answer::new(at(0), 0);
                assert_eq!(answer.next(Some(&at(0)), false), Step::Refuse(DIALOG_CHANGED));
            }

            #[test]
            fn a_selection_that_does_not_move_is_reported() {
                let mut answer = Answer::new(at(0), 2);
                let steps: Vec<Step> = (0..6).map(|_| answer.next(Some(&at(0)), true)).collect();
                assert_eq!(steps.iter().filter(|s| **s == Step::Press(Key::Down)).count(), 4);
                assert_eq!(steps.last(), Some(&Step::Refuse(NOT_MOVED)));
                assert!(!steps.contains(&Step::Press(Key::Enter)));
            }

            #[test]
            fn enter_that_leaves_the_dialog_open_is_reported() {
                let mut answer = Answer::new(at(0), 0);
                assert_eq!(answer.next(Some(&at(0)), true), Step::Press(Key::Enter));
                let last = (0..CHECKS_AFTER_ENTER)
                    .map(|_| answer.next(Some(&at(0)), true))
                    .last();
                assert_eq!(last, Some(Step::Refuse(STILL_OPEN)));
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn file(pid: u32, status: &str, tmux: &str) -> String {
            format!(
                r#"{{"pid":{pid},"sessionId":"s{pid}","status":"{status}","waitingFor":"permission prompt","statusUpdatedAt":1000,"tmux":"{tmux}","updatedAt":1000}}"#
            )
        }

        #[test]
        fn a_waiting_file_is_read_with_its_pane() {
            assert_eq!(
                parse_session_file(&file(7, "waiting", "home:@49.%49")),
                Some(Wait {
                    pane: "%49".to_owned(),
                    pid: 7,
                    label: "permission prompt".to_owned(),
                    updated_ms: 1000,
                })
            );
            let unlabeled = r#"{"pid":8,"status":"waiting","waitingFor":null,"tmux":"x:@1.%3"}"#;
            assert_eq!(
                parse_session_file(unlabeled).map(|wait| wait.label),
                Some("Waiting for you".to_owned())
            );
        }

        #[test]
        fn files_of_dead_processes_and_other_states_are_ignored() {
            for status in ["idle", "busy", "shell"] {
                assert_eq!(parse_session_file(&file(7, status, "home:@49.%49")), None);
            }
            assert_eq!(parse_session_file(&file(7, "waiting", "no-pane")), None);
            assert_eq!(parse_session_file("not json"), None);
            let waits = vec![parse_session_file(&file(7, "waiting", "home:@49.%49")).unwrap()];
            let panes = parse_pane_sessions("%49 $3\n");
            assert!(waits_by_session(waits, &panes, |_| false).is_empty());
        }

        #[test]
        fn waits_map_through_their_pane_to_its_session() {
            let panes = parse_pane_sessions("%1 $1\n%2 $1\n%3 $2\n");
            let mut first = parse_session_file(&file(10, "waiting", "a:@1.%2")).unwrap();
            first.updated_ms = 5;
            let mut newer = parse_session_file(&file(11, "waiting", "a:@1.%1")).unwrap();
            newer.updated_ms = 9;
            let elsewhere = parse_session_file(&file(12, "waiting", "b:@9.%9")).unwrap();
            let by_session = waits_by_session(vec![first, newer, elsewhere], &panes, |_| true);
            // Two panes of session $1 wait: the newer one speaks for it. $2 has none, and a pane
            // the server does not know (%9) maps nowhere.
            assert_eq!(by_session.len(), 1);
            assert_eq!(by_session["$1"].pid, 11);
        }
    }
}
