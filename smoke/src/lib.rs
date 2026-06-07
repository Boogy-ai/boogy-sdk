//! Minimal Boogy service. Copy this crate to start a project — see the
//! repo README for switching the path deps to git deps.

mod bindings {
    wit_bindgen::generate!({
        world: "service",
        path: "wit",
    });
}

boogy_sdk::wit_glue!(bindings, SmokeApi);

use boogy_sdk::Api;

struct SmokeApi;

impl Api for SmokeApi {
    fn build_router() -> Router {
        Router::new().get("/api/ping", ping)
    }
}

#[derive(Serialize)]
struct Pong {
    message: &'static str,
}

fn ping(_req: &mut Req<'_>) -> Json<Pong> {
    Json(Pong { message: "pong" })
}
