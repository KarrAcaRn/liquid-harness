"""Throwaway same-handler Axum adapter size probe (not production code)."""
from pathlib import Path
import shutil
import subprocess
import json
root=Path.cwd()
dest=root/'target/axum-size-probe'
dest.mkdir(exist_ok=True)
shutil.copytree(root/'crates/lh-cli/src',dest/'src',dirs_exist_ok=True)
manifest=(root/'crates/lh-cli/Cargo.toml').read_text().replace('version.workspace = true','version = "0.1.0"').replace('edition.workspace = true','edition = "2024"')
for crate in ['lh-core','lh-protocol']:
    manifest=manifest.replace(crate+'.workspace = true',crate+' = { path = '+json.dumps(str(root/'crates'/crate))+' }')
manifest=manifest.replace('tokio.workspace = true','tokio = { version="1", features=["rt","process","io-util","sync","time","macros"] }').replace('clap.workspace = true','clap = { version="4", features=["derive"] }').replace('serde.workspace = true','serde = { version="1", features=["derive"] }')
manifest=manifest.replace('features = ["tokio"]','features = ["tokio", "service"]')
manifest+='\naxum = { version="0.8", default-features=false, features=["http1","tokio"] }\n\n[workspace]\n[profile.release]\nlto="thin"\nstrip=true\nopt-level="s"\n'
(dest/'Cargo.toml').write_text(manifest)
source=(dest/'src/server.rs').read_text().replace('Frame, Incoming','Frame').replace('    service::service_fn,\n','').replace('Request<Incoming>','Request<axum::body::Body>')
source=source.replace('let service = service_fn(move |request| { let state = state.clone(); async move {','let service = axum::Router::new().fallback(move |request: Request<axum::body::Body>| { let state = state.clone(); async move {')
source=source.replace('Ok::<_, Infallible>(match handle(request, state).await { Ok(reply) => reply, Err((status, message)) => failure(status, &message) })','match handle(request, state).await { Ok(reply) => reply, Err((status, message)) => failure(status, &message) }')
source=source.replace('.serve_connection(TokioIo::new(socket), service)','.serve_connection(TokioIo::new(socket), hyper_util::service::TowerToHyperService::new(service))')
(dest/'src/server.rs').write_text(source)
subprocess.run(['cargo','build','--release','--manifest-path',str(dest/'Cargo.toml')],check=True)
print(json.dumps({'hyper_bytes':(root/'target/release/lh').stat().st_size,'axum_adapter_bytes':(dest/'target/release/lh').stat().st_size}))
