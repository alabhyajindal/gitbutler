use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use tauri::AppHandle;
use tokio::sync::Semaphore;

use crate::{
    gb_repository, git, keys,
    paths::DataDir,
    project_repository::{self, conflicts},
    projects, users,
};

use super::{branch::Ownership, RemoteBranchFile};

pub struct Controller {
    local_data_dir: DataDir,
    semaphores: Arc<tokio::sync::Mutex<HashMap<String, Semaphore>>>,

    projects: projects::Controller,
    users: users::Controller,
    keys: keys::Controller,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("detached head")]
    GetProject(#[from] projects::GetError),
    #[error(transparent)]
    OpenProjectRepository(#[from] project_repository::OpenError),
    #[error(transparent)]
    Verify(#[from] super::integration::VerifyError),
    #[error(transparent)]
    ProjectRemote(#[from] project_repository::RemoteError),
    #[error("project is in a conflicted state")]
    Conflicting,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl TryFrom<&AppHandle> for Controller {
    type Error = Error;

    fn try_from(value: &AppHandle) -> Result<Self, Self::Error> {
        let local_data_dir = DataDir::try_from(value)?;
        Ok(Self {
            local_data_dir,
            semaphores: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            projects: projects::Controller::try_from(value)?,
            users: users::Controller::from(value),
            keys: keys::Controller::from(value),
        })
    }
}

impl Controller {
    pub async fn create_commit(
        &self,
        project_id: &str,
        branch: &str,
        message: &str,
        ownership: Option<&Ownership>,
    ) -> Result<(), Error> {
        self.with_lock(project_id, || {
            self.with_verify_branch(project_id, |gb_repository, project_repository, user| {
                if conflicts::is_conflicting(project_repository, None)
                    .context("failed to check for conflicts")?
                {
                    return Err(Error::Conflicting);
                }

                let signing_key = if project_repository
                    .config()
                    .sign_commits()
                    .context("failed to get sign commits option")?
                {
                    Some(
                        self.keys
                            .get_or_create()
                            .context("failed to get private key")?,
                    )
                } else {
                    None
                };
                super::commit(
                    gb_repository,
                    project_repository,
                    branch,
                    message,
                    ownership,
                    signing_key.as_ref(),
                    user,
                )?;
                Ok(())
            })
        })
        .await
    }

    pub fn can_apply_remote_branch(
        &self,
        project_id: &str,
        branch_name: &git::BranchName,
    ) -> Result<bool, Error> {
        let project = self.projects.get(project_id)?;
        let project_repository = project_repository::Repository::try_from(&project)?;
        let user = self.users.get_user().context("failed to get user")?;
        let gb_repository = gb_repository::Repository::open(
            &self.local_data_dir,
            &project_repository,
            user.as_ref(),
        )
        .context("failed to open gitbutler repository")?;
        super::is_remote_branch_mergeable(&gb_repository, &project_repository, branch_name)
            .map_err(Error::Other)
    }

    pub fn can_apply_virtual_branch(
        &self,
        project_id: &str,
        branch_id: &str,
    ) -> Result<bool, Error> {
        let project = self.projects.get(project_id)?;
        let project_repository = project_repository::Repository::try_from(&project)?;
        let user = self.users.get_user().context("failed to get user")?;
        let gb_repository = gb_repository::Repository::open(
            &self.local_data_dir,
            &project_repository,
            user.as_ref(),
        )
        .context("failed to open gitbutler repository")?;
        super::is_virtual_branch_mergeable(&gb_repository, &project_repository, branch_id)
            .map_err(Error::Other)
    }

    pub async fn list_virtual_branches(
        &self,
        project_id: &str,
    ) -> Result<Vec<super::VirtualBranch>, Error> {
        self.with_lock(project_id, || {
            self.with_verify_branch(project_id, |gb_repository, project_repository, _| {
                super::list_virtual_branches(gb_repository, project_repository)
                    .map_err(Error::Other)
            })
        })
        .await
    }

    pub async fn create_virtual_branch(
        &self,
        project_id: &str,
        create: &super::branch::BranchCreateRequest,
    ) -> Result<(), Error> {
        self.with_lock(project_id, || {
            self.with_verify_branch(project_id, |gb_repository, project_repository, _| {
                if conflicts::is_resolving(project_repository) {
                    return Err(Error::Conflicting);
                }
                super::create_virtual_branch(gb_repository, create).map_err(Error::Other)?;
                Ok(())
            })
        })
        .await
    }

    pub async fn create_virtual_branch_from_branch(
        &self,
        project_id: &str,
        branch: &git::BranchName,
    ) -> Result<String, Error> {
        self.with_lock::<Result<String, Error>>(project_id, || {
            self.with_verify_branch(project_id, |gb_repository, project_repository, user| {
                let branch = super::create_virtual_branch_from_branch(
                    gb_repository,
                    project_repository,
                    branch,
                    None,
                    user,
                )
                .map_err(Error::Other)?;

                let signing_key = if project_repository
                    .config()
                    .sign_commits()
                    .context("failed to get sign commits option")?
                {
                    Some(
                        self.keys
                            .get_or_create()
                            .context("failed to get private key")?,
                    )
                } else {
                    None
                };

                // also apply the branch
                super::apply_branch(
                    gb_repository,
                    project_repository,
                    &branch.id,
                    signing_key.as_ref(),
                    user,
                )
                .map_err(Error::Other)?;
                Ok(branch.id)
            })
        })
        .await
    }

    pub fn get_base_branch_data(
        &self,
        project_id: &str,
    ) -> Result<Option<super::BaseBranch>, Error> {
        let project = self.projects.get(project_id)?;
        let project_repository = project_repository::Repository::try_from(&project)?;
        let user = self.users.get_user().context("failed to get user")?;
        let gb_repository = gb_repository::Repository::open(
            &self.local_data_dir,
            &project_repository,
            user.as_ref(),
        )
        .context("failed to open gitbutler repository")
        .map_err(Error::Other)?;
        let base_branch = super::get_base_branch_data(&gb_repository, &project_repository)?;
        Ok(base_branch)
    }

    pub fn list_remote_commit_files(
        &self,
        project_id: &str,
        commit_oid: git::Oid,
    ) -> Result<Vec<RemoteBranchFile>, Error> {
        let project = self.projects.get(project_id)?;
        let project_repository = project_repository::Repository::try_from(&project)?;
        let commit = project_repository
            .git_repository
            .find_commit(commit_oid)
            .context("failed to find commit")?;
        super::list_remote_commit_files(&project_repository.git_repository, &commit)
            .map_err(Error::Other)
    }

    pub fn set_base_branch(
        &self,
        project_id: &str,
        target_branch: &git::RemoteBranchName,
    ) -> Result<super::BaseBranch, Error> {
        let project = self.projects.get(project_id)?;

        let user = self.users.get_user().context("failed to get user")?;

        let project_repository = project_repository::Repository::try_from(&project)?;

        let gb_repository = gb_repository::Repository::open(
            &self.local_data_dir,
            &project_repository,
            user.as_ref(),
        )
        .context("failed to open gitbutler repository")?;

        let target = super::set_base_branch(
            &gb_repository,
            &project_repository,
            user.as_ref(),
            target_branch,
        )?;

        Ok(target)
    }

    pub async fn merge_virtual_branch_upstream(
        &self,
        project_id: &str,
        branch: &str,
    ) -> Result<(), Error> {
        self.with_lock(project_id, || {
            self.with_verify_branch(project_id, |gb_repository, project_repository, user| {
                if conflicts::is_conflicting(project_repository, None)
                    .context("failed to check for conflicts")?
                {
                    return Err(Error::Conflicting);
                }

                let signing_key = if project_repository
                    .config()
                    .sign_commits()
                    .context("failed to get sign commits option")?
                {
                    Some(
                        self.keys
                            .get_or_create()
                            .context("failed to get private key")?,
                    )
                } else {
                    None
                };
                super::merge_virtual_branch_upstream(
                    gb_repository,
                    project_repository,
                    branch,
                    signing_key.as_ref(),
                    user,
                )
                .map_err(Error::Other)
            })
        })
        .await
    }

    pub async fn update_base_branch(&self, project_id: &str) -> Result<(), Error> {
        self.with_lock(project_id, || {
            self.with_verify_branch(project_id, |gb_repository, project_repository, user| {
                super::update_base_branch(gb_repository, project_repository, user)
                    .map_err(Error::Other)
            })
        })
        .await
    }

    pub async fn update_virtual_branch(
        &self,
        project_id: &str,
        branch_update: super::branch::BranchUpdateRequest,
    ) -> Result<(), Error> {
        self.with_lock(project_id, || {
            self.with_verify_branch(project_id, |gb_repository, project_repository, _| {
                super::update_branch(gb_repository, project_repository, branch_update)?;
                Ok(())
            })
        })
        .await
    }

    pub async fn delete_virtual_branch(
        &self,
        project_id: &str,
        branch_id: &str,
    ) -> Result<(), Error> {
        self.with_lock(project_id, || {
            self.with_verify_branch(project_id, |gb_repository, project_repository, _| {
                super::delete_branch(gb_repository, project_repository, branch_id)?;
                Ok(())
            })
        })
        .await
    }

    pub async fn apply_virtual_branch(
        &self,
        project_id: &str,
        branch_id: &str,
    ) -> Result<(), Error> {
        self.with_lock(project_id, || {
            self.with_verify_branch(project_id, |gb_repository, project_repository, user| {
                let signing_key = if project_repository
                    .config()
                    .sign_commits()
                    .context("failed to get sign commits option")?
                {
                    Some(
                        self.keys
                            .get_or_create()
                            .context("failed to get private key")?,
                    )
                } else {
                    None
                };
                super::apply_branch(
                    gb_repository,
                    project_repository,
                    branch_id,
                    signing_key.as_ref(),
                    user,
                )
                .map_err(Error::Other)
            })
        })
        .await
    }

    pub async fn unapply_ownership(
        &self,
        project_id: &str,
        ownership: &Ownership,
    ) -> Result<(), Error> {
        self.with_lock(project_id, || {
            self.with_verify_branch(project_id, |gb_repository, project_repository, _| {
                super::unapply_ownership(gb_repository, project_repository, ownership)
                    .map_err(Error::Other)
            })
        })
        .await
    }

    pub async fn reset_virtual_branch(
        &self,
        project_id: &str,
        branch_id: &str,
        target_commit_oid: git::Oid,
    ) -> Result<(), Error> {
        self.with_lock(project_id, || {
            self.with_verify_branch(project_id, |gb_repository, project_repository, _| {
                super::reset_branch(
                    gb_repository,
                    project_repository,
                    branch_id,
                    target_commit_oid,
                )
                .map_err(Error::Other)
            })
        })
        .await
    }

    pub async fn unapply_virtual_branch(
        &self,
        project_id: &str,
        branch_id: &str,
    ) -> Result<(), Error> {
        self.with_lock(project_id, || {
            self.with_verify_branch(project_id, |gb_repository, project_repository, _| {
                super::unapply_branch(gb_repository, project_repository, branch_id)
                    .map_err(Error::Other)
            })
        })
        .await
    }

    pub async fn push_virtual_branch(
        &self,
        project_id: &str,
        branch_id: &str,
        with_force: bool,
    ) -> Result<(), Error> {
        self.with_lock(project_id, || {
            self.with_verify_branch(project_id, |gb_repository, project_repository, _| {
                let private_key = match &project_repository.project().preferred_key {
                    projects::AuthKey::Local {
                        private_key_path,
                        passphrase,
                    } => keys::Key::Local {
                        private_key_path: private_key_path.clone(),
                        passphrase: passphrase.clone(),
                    },
                    projects::AuthKey::Generated => {
                        let private_key = self
                            .keys
                            .get_or_create()
                            .context("failed to get or create private key")?;
                        keys::Key::Generated(Box::new(private_key))
                    }
                };

                super::push(
                    project_repository,
                    gb_repository,
                    branch_id,
                    with_force,
                    &private_key,
                )
                .map_err(|e| match e {
                    super::PushError::Remote(error) => Error::ProjectRemote(error),
                    other => Error::Other(anyhow::Error::from(other)),
                })
            })
        })
        .await
    }

    fn with_verify_branch<T>(
        &self,
        project_id: &str,
        action: impl FnOnce(
            &gb_repository::Repository,
            &project_repository::Repository,
            Option<&users::User>,
        ) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let project = self.projects.get(project_id)?;
        let project_repository = project_repository::Repository::try_from(&project)?;
        let user = self.users.get_user().context("failed to get user")?;
        let gb_repository = gb_repository::Repository::open(
            &self.local_data_dir,
            &project_repository,
            user.as_ref(),
        )
        .context("failed to open gitbutler repository")
        .map_err(Error::Other)?;
        super::integration::verify_branch(&gb_repository, &project_repository)?;
        action(&gb_repository, &project_repository, user.as_ref())
    }

    async fn with_lock<T>(&self, project_id: &str, action: impl FnOnce() -> T) -> T {
        let mut semaphores = self.semaphores.lock().await;
        let semaphore = semaphores
            .entry(project_id.to_string())
            .or_insert_with(|| Semaphore::new(1));
        let _permit = semaphore.acquire().await;
        action()
    }
}
