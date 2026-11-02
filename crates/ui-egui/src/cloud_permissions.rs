//! Effective edit permissions for files shared through CraftCloud.

use uuid::Uuid;

/// A grant returned by the organization's shared-with-me endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantPermission {
    pub target_kind: String,
    pub target_id: Uuid,
    pub role: String,
}

/// Check whether the current organization member may edit a file.
///
/// `grants` must contain only active grants for the current user.
pub fn file_is_editable(org_role: &str, workspace_role: Option<&str>, is_guest: bool, file_id: Uuid, project_id: Uuid, grants: &[GrantPermission]) -> bool {
    role_is(org_role, &["owner", "admin"])
        || (!is_guest && workspace_role.is_some_and(|role| role_is(role, &["admin", "editor"])))
        || grants.iter().any(|grant| {
            role_is(&grant.role, &["editor"])
                && ((grant.target_kind == "file" && grant.target_id == file_id) || (grant.target_kind == "project" && grant.target_id == project_id))
        })
}

fn role_is(role: &str, expected: &[&str]) -> bool {
    expected.iter().any(|expected| role.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
    use super::{GrantPermission, file_is_editable};
    use uuid::Uuid;

    fn grant(target_kind: &str, target_id: Uuid, role: &str) -> GrantPermission {
        GrantPermission { target_kind: target_kind.into(), target_id, role: role.into() }
    }

    #[test]
    fn organization_owner_and_admin_can_edit() {
        let file_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        for role in ["owner", "admin"] {
            assert!(file_is_editable(role, None, true, file_id, project_id, &[]));
        }
    }

    #[test]
    fn workspace_admin_and_editor_can_edit_when_not_guest() {
        let file_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        for role in ["admin", "editor"] {
            assert!(file_is_editable("member", Some(role), false, file_id, project_id, &[]));
        }
    }

    #[test]
    fn matching_file_and_project_editor_grants_can_edit() {
        let file_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        for (kind, id) in [("file", file_id), ("project", project_id)] {
            assert!(file_is_editable("member", None, false, file_id, project_id, &[grant(kind, id, "editor")]));
        }
    }

    #[test]
    fn guest_can_edit_with_editor_grant_but_not_workspace_role() {
        let file_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        assert!(file_is_editable("member", None, true, file_id, project_id, &[grant("file", file_id, "editor")]));
        assert!(!file_is_editable("member", Some("editor"), true, file_id, project_id, &[]));
    }

    #[test]
    fn viewers_mismatched_grants_and_missing_grants_cannot_edit() {
        let file_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        assert!(!file_is_editable("viewer", Some("viewer"), false, file_id, project_id, &[]));
        assert!(!file_is_editable("member", None, false, file_id, project_id, &[grant("file", Uuid::new_v4(), "editor")]));
        assert!(!file_is_editable("member", None, false, file_id, project_id, &[grant("workspace", file_id, "editor")]));
        assert!(!file_is_editable("member", None, false, file_id, project_id, &[]));
    }
}
