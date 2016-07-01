// Copyright (c) 2016 Chef Software Inc. and/or applicable contributors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! A collection of handlers for the HTTP server's router

use bodyparser;
use hab_core::package::Plan;
use hab_net;
use hab_net::routing::Broker;
use iron::headers::ContentType;
use iron::mime::{Mime, TopLevel, SubLevel};
use iron::modifiers::Header;
use iron::prelude::*;
use iron::status;
use persistent;
use protocol::jobsrv::{Job, JobGet, JobSpec};
use protocol::sessionsrv::{OAuthProvider, Session, SessionCreate};
use protocol::vault::*;
use protocol::net::{self, NetError, NetOk, ErrCode};
use router::Router;
use rustc_serialize::base64::FromBase64;
use rustc_serialize::json::{self, ToJson};
use serde_json::Value;

use super::super::server::ZMQ_CONTEXT;
use super::middleware::*;
use super::GitHubCli;

pub fn session_create(req: &mut Request) -> IronResult<Response> {
    let code = {
        let params = req.extensions.get::<Router>().unwrap();
        match params.find("code") {
            Some(code) => code.to_string(),
            _ => return Ok(Response::with(status::BadRequest)),
        }
    };
    let github = req.get::<persistent::Read<GitHubCli>>().unwrap();
    match github.authenticate(&code) {
        Ok(token) => {
            match github.user(&token) {
                Ok(user) => {
                    // Select primary email. If no primary email can be found, use any email. If
                    // no email is associated with account return an access denied error.
                    let email = match github.emails(&token) {
                        Ok(ref emails) => {
                            emails.iter().find(|e| e.primary).unwrap_or(&emails[0]).email.clone()
                        }
                        Err(_) => {
                            let err = net::err(ErrCode::ACCESS_DENIED, "rg:auth:0");
                            return Ok(render_net_error(&err));
                        }
                    };
                    let mut conn = Broker::connect(&**ZMQ_CONTEXT).unwrap();
                    let mut request = SessionCreate::new();
                    request.set_token(token);
                    request.set_extern_id(user.id);
                    request.set_email(email);
                    request.set_name(user.login);
                    request.set_provider(OAuthProvider::GitHub);
                    match conn.route::<SessionCreate, Session>(&request) {
                        Ok(session) => Ok(render_json(status::Ok, &session)),
                        Err(err) => Ok(render_net_error(&err)),
                    }
                }
                Err(e @ hab_net::Error::JsonDecode(_)) => {
                    debug!("github user get, err={:?}", e);
                    let err = net::err(ErrCode::BAD_REMOTE_REPLY, "rg:auth:1");
                    Ok(render_net_error(&err))
                }
                Err(e) => {
                    debug!("github user get, err={:?}", e);
                    let err = net::err(ErrCode::BUG, "rg:auth:2");
                    Ok(render_net_error(&err))
                }
            }
        }
        Err(hab_net::Error::Auth(e)) => {
            debug!("github authentication, err={:?}", e);
            let err = net::err(ErrCode::REMOTE_REJECTED, e.error);
            Ok(render_net_error(&err))
        }
        Err(e @ hab_net::Error::JsonDecode(_)) => {
            debug!("github authentication, err={:?}", e);
            let err = net::err(ErrCode::BAD_REMOTE_REPLY, "rg:auth:1");
            Ok(render_net_error(&err))
        }
        Err(e) => {
            error!("github authentication, err={:?}", e);
            let err = net::err(ErrCode::BUG, "rg:auth:0");
            Ok(render_net_error(&err))
        }
    }
}

pub fn job_create(req: &mut Request) -> IronResult<Response> {
    let mut project_get = ProjectGet::new();
    {
        match req.get::<bodyparser::Json>() {
            Ok(Some(body)) => {
                match body.find("project_id") {
                    Some(&Value::String(ref val)) => project_get.set_id(val.to_string()),
                    _ => return Ok(Response::with(status::BadRequest)),
                }
            }
            _ => return Ok(Response::with(status::BadRequest)),
        }
    }
    let session = req.extensions.get::<Authenticated>().unwrap();
    let mut conn = Broker::connect(&**ZMQ_CONTEXT).unwrap();
    let project = match conn.route::<ProjectGet, Project>(&project_get) {
        Ok(project) => project,
        Err(err) => return Ok(render_net_error(&err)),
    };
    let mut job_spec: JobSpec = JobSpec::new();
    job_spec.set_owner_id(session.get_id());
    job_spec.set_project(project);
    match conn.route::<JobSpec, Job>(&job_spec) {
        Ok(job) => Ok(render_json(status::Created, &job)),
        Err(err) => Ok(render_net_error(&err)),
    }
}

pub fn job_show(req: &mut Request) -> IronResult<Response> {
    let params = req.extensions.get::<Router>().unwrap();
    let id = match params.find("id") {
        Some(id) => {
            match id.parse() {
                Ok(id) => id,
                Err(_) => return Ok(Response::with(status::BadRequest)),
            }
        }
        _ => return Ok(Response::with(status::BadRequest)),
    };
    let mut conn = Broker::connect(&**ZMQ_CONTEXT).unwrap();
    let mut request = JobGet::new();
    request.set_id(id);
    match conn.route::<JobGet, Job>(&request) {
        Ok(job) => Ok(render_json(status::Ok, &job)),
        Err(err) => Ok(render_net_error(&err)),
    }
}

/// Endpoint for determining availability of builder-api components.
///
/// Returns a status 200 on success. Any non-200 responses are an outage or a partial outage.
pub fn status(_req: &mut Request) -> IronResult<Response> {
    Ok(Response::with(status::Ok))
}

fn render_json<T: ToJson>(status: status::Status, response: &T) -> Response {
    let encoded = json::encode(&response.to_json()).unwrap();
    let headers = Header(ContentType(Mime(TopLevel::Application, SubLevel::Json, vec![])));
    Response::with((status, encoded, headers))
}

/// Return an IronResult containing the body of a NetError and the appropriate HTTP response status
/// for the corresponding NetError.
///
/// For example, a NetError::ENTITY_NOT_FOUND will result in an HTTP response containing the body
/// of the NetError with an HTTP status of 404.
///
/// # Panics
///
/// * The given encoded message was not a NetError
/// * The given messsage could not be decoded
/// * The NetError could not be encoded to JSON
fn render_net_error(err: &NetError) -> Response {
    let status = match err.get_code() {
        ErrCode::ENTITY_NOT_FOUND => status::NotFound,
        ErrCode::ENTITY_CONFLICT => status::Conflict,
        ErrCode::NO_SHARD => status::ServiceUnavailable,
        ErrCode::TIMEOUT => status::RequestTimeout,
        ErrCode::BAD_REMOTE_REPLY => status::BadGateway,
        ErrCode::SESSION_EXPIRED => status::Unauthorized,
        _ => status::InternalServerError,
    };
    render_json(status, err)
}

pub fn list_account_invitations(req: &mut Request) -> IronResult<Response> {
    let session = req.extensions.get::<Authenticated>().unwrap();
    let mut conn = Broker::connect(&**ZMQ_CONTEXT).unwrap();
    let mut request = AccountInvitationListRequest::new();
    request.set_account_id(session.get_id());
    match conn.route::<AccountInvitationListRequest, AccountInvitationListResponse>(&request) {
        Ok(invites) => Ok(render_json(status::Ok, &invites)),
        Err(err) => Ok(render_net_error(&err)),
    }
}

pub fn list_user_origins(req: &mut Request) -> IronResult<Response> {
    let session = req.extensions.get::<Authenticated>().unwrap();
    let mut conn = Broker::connect(&**ZMQ_CONTEXT).unwrap();
    let mut request = AccountOriginListRequest::new();
    request.set_account_id(session.get_id());
    match conn.route::<AccountOriginListRequest, AccountOriginListResponse>(&request) {
        Ok(invites) => Ok(render_json(status::Ok, &invites)),
        Err(err) => Ok(render_net_error(&err)),
    }
}

pub fn accept_invitation(req: &mut Request) -> IronResult<Response> {
    let session = req.extensions.get::<Authenticated>().unwrap();
    let params = &req.extensions.get::<Router>().unwrap();
    let invitation_id = match params.find("invitation_id") {
        Some(ref invitation_id) => {
            match invitation_id.parse::<u64>() {
                Ok(v) => v,
                Err(_) => return Ok(Response::with(status::BadRequest)),
            }
        }
        None => return Ok(Response::with(status::BadRequest)),
    };

    // TODO: read the body to determine "ignore"
    let ignore_val = false;

    let mut conn = Broker::connect(&**ZMQ_CONTEXT).unwrap();
    let mut request = OriginInvitationAcceptRequest::new();

    // make sure we're not trying to accept someone else's request
    request.set_account_accepting_request(session.get_id());
    request.set_invite_id(invitation_id);
    request.set_ignore(ignore_val);
    match conn.route::<OriginInvitationAcceptRequest, OriginInvitationAcceptResponse>(&request) {
        Ok(_invites) => Ok(Response::with(status::NoContent)),
        Err(err) => Ok(render_net_error(&err)),
    }
}

/// Create a new project as the authenticated user and associated to the given origin
pub fn project_create(req: &mut Request) -> IronResult<Response> {
    let mut project = ProjectCreate::new();
    let mut origin_get = OriginGet::new();
    let github = req.get::<persistent::Read<GitHubCli>>().unwrap();
    let session = req.extensions.get::<Authenticated>().unwrap().clone();
    let (organization, repo): (String, String) = {
        match req.get::<bodyparser::Json>() {
            Ok(Some(body)) => {
                match body.find("origin") {
                    Some(&Value::String(ref val)) => origin_get.set_name(val.to_string()),
                    _ => {
                        return Ok(Response::with((status::BadRequest,
                                                  "Missing required field: `origin`")))
                    }
                }
                match body.find("plan_path") {
                    Some(&Value::String(ref val)) => project.set_plan_path(val.to_string()),
                    _ => {
                        return Ok(Response::with((status::BadRequest,
                                                  "Missing required field: `plan_path`")))
                    }
                }
                match body.find("github") {
                    Some(&Value::Object(ref map)) => {
                        let mut vcs = VCSGit::new();
                        let organization = match map.get("organization") {
                            Some(&Value::String(ref val)) => val.to_string(),
                            _ => {
                                return Ok(Response::with((status::BadRequest,
                                                          "Missing required field: \
                                                           `github.organization`")))
                            }
                        };
                        let repo = match map.get("repo") {
                            Some(&Value::String(ref val)) => val.to_string(),
                            _ => {
                                return Ok(Response::with((status::BadRequest,
                                                          "Missing required field: `github.repo`")))
                            }
                        };
                        match github.repo(&session.get_token(), &organization, &repo) {
                            Ok(repo) => vcs.set_url(repo.clone_url),
                            Err(_) => return Ok(Response::with((status::BadRequest, "rg:pc:1"))),
                        }
                        project.set_git(vcs);
                        (organization, repo)
                    }
                    _ => {
                        return Ok(Response::with((status::BadRequest,
                                                  "Missing required field: `github`")))
                    }
                }
            }
            _ => return Ok(Response::with(status::BadRequest)),
        }
    };
    let mut conn = Broker::connect(&**ZMQ_CONTEXT).unwrap();
    let origin = match conn.route::<OriginGet, Origin>(&origin_get) {
        Ok(response) => response,
        Err(err) => return Ok(render_net_error(&err)),
    };
    match github.contents(&session.get_token(),
                          &organization,
                          &repo,
                          &project.get_plan_path()) {
        Ok(contents) => {
            match contents.content.from_base64() {
                Ok(ref bytes) => {
                    match Plan::from_bytes(bytes) {
                        Ok(plan) => project.set_id(format!("{}/{}", origin.get_name(), plan.name)),
                        Err(_) => return Ok(Response::with((status::BadRequest, "rg:pc:3"))),
                    }
                }
                Err(_) => return Ok(Response::with((status::BadRequest, "rg:pc:4"))),
            }
        }
        Err(_) => return Ok(Response::with((status::BadRequest, "rg:pc:2"))),
    }
    project.set_owner_id(session.get_id());
    match conn.route::<ProjectCreate, Project>(&project) {
        Ok(response) => Ok(render_json(status::Created, &response)),
        Err(err) => Ok(render_net_error(&err)),
    }
}

/// Delete the given project
pub fn project_delete(req: &mut Request) -> IronResult<Response> {
    let mut project_del = ProjectDelete::new();
    let params = req.extensions.get::<Router>().unwrap();
    match params.find("id") {
        Some(id) => {
            match id.parse() {
                Ok(id) => project_del.set_id(id),
                Err(_) => return Ok(Response::with(status::BadRequest)),
            }
        }
        _ => return Ok(Response::with(status::BadRequest)),
    };
    let session = req.extensions.get::<Authenticated>().unwrap();
    project_del.set_requestor_id(session.get_id());
    let mut conn = Broker::connect(&**ZMQ_CONTEXT).unwrap();
    match conn.route::<ProjectDelete, NetOk>(&project_del) {
        Ok(_) => Ok(Response::with(status::NoContent)),
        Err(err) => Ok(render_net_error(&err)),
    }
}

/// Update the given project
pub fn project_update(req: &mut Request) -> IronResult<Response> {
    let mut project_up = ProjectUpdate::new();
    let mut project = Project::new();
    let params = req.extensions.get::<Router>().unwrap();
    // JW TODO: parse actual body
    match params.find("id") {
        Some(id) => {
            match id.parse() {
                Ok(id) => project.set_id(id),
                Err(_) => return Ok(Response::with(status::BadRequest)),
            }
        }
        _ => return Ok(Response::with(status::BadRequest)),
    };
    let session = req.extensions.get::<Authenticated>().unwrap();
    project_up.set_requestor_id(session.get_id());
    project_up.set_project(project);
    let mut conn = Broker::connect(&**ZMQ_CONTEXT).unwrap();
    match conn.route::<ProjectUpdate, NetOk>(&project_up) {
        Ok(_) => Ok(Response::with(status::NoContent)),
        Err(err) => Ok(render_net_error(&err)),
    }
}

/// Display the the given project's details
pub fn project_show(req: &mut Request) -> IronResult<Response> {
    let mut project_get = ProjectGet::new();
    let params = req.extensions.get::<Router>().unwrap();
    match params.find("id") {
        Some(id) => {
            match id.parse() {
                Ok(id) => project_get.set_id(id),
                Err(_) => return Ok(Response::with(status::BadRequest)),
            }
        }
        _ => return Ok(Response::with(status::BadRequest)),
    };
    let mut conn = Broker::connect(&**ZMQ_CONTEXT).unwrap();
    match conn.route::<ProjectGet, Project>(&project_get) {
        Ok(project) => Ok(render_json(status::Ok, &project)),
        Err(err) => Ok(render_net_error(&err)),
    }
}
