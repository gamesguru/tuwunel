#![cfg(test)]

use std::{env::temp_dir, fs::remove_dir_all};

use tuwunel::{Args, Runtime, Server, async_exec};
use tuwunel_core::{
	Result,
	pdu::PduBuilder,
	ruma::{
		RoomId, RoomVersionId,
		events::room::{
			create::RoomCreateEventContent,
			member::{MembershipState, RoomMemberEventContent},
		},
	},
};

async fn create_test_room(
	services: &tuwunel_service::Services,
	room_id: &RoomId,
	room_version: RoomVersionId,
) -> Result<()> {
	let _short_id = services
		.short
		.get_or_create_shortroomid(room_id)
		.await;

	let state_lock = services.state.mutex.lock(room_id).await;

	let server_user = services.globals.server_user.as_ref();
	if !services.users.exists(server_user).await {
		services
			.users
			.create(server_user, None, None)
			.await?;
	}

	let create_content = {
		use RoomVersionId::*;
		match room_version {
			| V1 | V2 | V3 | V4 | V5 | V6 | V7 | V8 | V9 | V10 =>
				RoomCreateEventContent::new_v1(server_user.into()),
			| _ => RoomCreateEventContent::new_v11(),
		}
	};

	// 1. The room create event
	services
		.timeline
		.build_and_append_pdu(
			PduBuilder::state(String::new(), &RoomCreateEventContent {
				federate: false,
				predecessor: None,
				room_version,
				..create_content
			}),
			server_user,
			room_id,
			&state_lock,
		)
		.await?;

	// 2. Make server user join
	services
		.timeline
		.build_and_append_pdu(
			PduBuilder::state(
				String::from(server_user),
				&RoomMemberEventContent::new(MembershipState::Join),
			),
			server_user,
			room_id,
			&state_lock,
		)
		.await?;

	Ok(())
}

#[test]
fn test_get_room_dag_versions() -> Result<()> {
	let db_dir = temp_dir().join("tuwunel-get-room-dag-versions-test");
	remove_dir_all(&db_dir).ok();

	let mut args = Args::default_test(&["smoke", "fresh", "cleanup"]);
	args.option
		.push(format!("database_path={:?}", db_dir.to_str().expect("utf-8 path")));

	let runtime = Runtime::new(Some(&args))?;
	let server = Server::new(Some(&args), Some(&runtime))?;

	let result = runtime.block_on(async {
		let services = server.services.lock().await;
		let services_ref = services.as_ref().expect("services initialized");

		let room_v5 = RoomId::new_v1(services_ref.globals.server_name());
		create_test_room(services_ref, &room_v5, RoomVersionId::V5).await?;

		let room_v11 = RoomId::new_v1(services_ref.globals.server_name());
		create_test_room(services_ref, &room_v11, RoomVersionId::V11).await?;

		let room_v12 = RoomId::new_v1(services_ref.globals.server_name());
		create_test_room(services_ref, &room_v12, RoomVersionId::V12).await?;

		Ok((room_v5, room_v11, room_v12))
	});

	drop(runtime);

	let (room_v5, room_v11, room_v12) = result?;

	// Now recreate args/server to execute the commands
	let mut args_exec = Args::default_test(&["smoke", "cleanup"]);
	args_exec
		.option
		.push(format!("database_path={:?}", db_dir.to_str().expect("utf-8 path")));
	args_exec
		.execute
		.push(format!("debug get-room-dag {} 0 -1", room_v5));
	args_exec
		.execute
		.push(format!("debug get-room-dag {} 0 -1", room_v11));
	args_exec
		.execute
		.push(format!("debug get-room-dag {} 0 -1", room_v12));

	let runtime_exec = Runtime::new(Some(&args_exec))?;
	let server_exec = Server::new(Some(&args_exec), Some(&runtime_exec))?;
	let exec_result = runtime_exec.block_on(async { async_exec(&server_exec).await });

	drop(runtime_exec);
	remove_dir_all(&db_dir).ok();

	assert!(exec_result.is_ok(), "Admin command execution failed: {:?}", exec_result.err());

	Ok(())
}
