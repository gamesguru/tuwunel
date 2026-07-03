use ruma::{
	RoomId, UserId,
	events::{GlobalAccountDataEventType, push_rules::PushRulesEvent},
	push::{AnyPushRuleRef, NewPushRule, NewSimplePushRule, RuleKind},
};
use tuwunel_core::{Result, err, implement};

#[implement(super::Service)]
pub async fn copy_room_push_rule(
	&self,
	user_id: &UserId,
	from_room: &RoomId,
	to_room: &RoomId,
) -> Result {
	let Ok(mut account_data): Result<PushRulesEvent> = self
		.get_global(user_id, GlobalAccountDataEventType::PushRules)
		.await
	else {
		return Ok(());
	};

	let ruleset = &mut account_data.content.global;

	let Some(AnyPushRuleRef::Room(rule)) = ruleset.get(RuleKind::Room, from_room) else {
		return Ok(());
	};

	let actions = rule.actions.clone();
	let rule = NewPushRule::Room(NewSimplePushRule::new(to_room.to_owned(), actions));

	ruleset
		.insert(rule, None, None)
		.map_err(|e| err!(Request(InvalidParam("Failed to copy room push rule: {e}"))))?;

	let ty = GlobalAccountDataEventType::PushRules;
	let data = serde_json::to_value(account_data)?;

	self.update(None, user_id, ty.to_string().into(), &data)
		.await
}
