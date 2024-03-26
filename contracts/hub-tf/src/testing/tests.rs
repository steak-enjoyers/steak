use std::str::FromStr;

use cosmwasm_std::{testing::{mock_env, mock_info, MockApi, MockStorage, MOCK_CONTRACT_ADDR},  Addr, BankMsg, Coin, CosmosMsg, Decimal, DistributionMsg, Order, OwnedDeps, ReplyOn, StdError, SubMsg, Uint128, WasmMsg, to_json_binary};
use pfc_steak::{
    hub::{
        Batch, CallbackMsg, ConfigResponse, PendingBatch, QueryMsg, StateResponse, UnbondRequest,
        UnbondRequestsByBatchResponseItem, UnbondRequestsByUserResponseItem,
    },
    hub_tf::{ExecuteMsg, InstantiateMsg},
};

use super::{
    custom_querier::CustomQuerier,
    helpers::{mock_dependencies, mock_env_at_timestamp, query_helper},
};
use crate::{
    contract::{execute, instantiate, REPLY_REGISTER_RECEIVED_COINS},
    helpers::{parse_coin, parse_received_fund},
    math::{
        compute_redelegations_for_rebalancing, compute_redelegations_for_removal,
        compute_undelegations,
    },
    state::{previous_batches, unbond_requests, State, VALIDATORS},
    token_factory::{
        denom,
        denom::{MsgBurn, MsgCreateDenom, MsgMint},
    },
    types::{Coins, Delegation, Redelegation, Undelegation},
};

//--------------------------------------------------------------------------------------------------
// Test setup
//--------------------------------------------------------------------------------------------------

fn setup_test() -> OwnedDeps<MockStorage, MockApi, CustomQuerier> {
    let mut deps = mock_dependencies();

    let res = instantiate(
        deps.as_mut(),
        mock_env_at_timestamp(10000),
        mock_info("deployer", &[]),
        InstantiateMsg {
            owner: "larry".to_string(),
            denom: "uxyz".to_string(),
            steak_denom: "boneXYZ".to_string(),
            fee_account_type: "Wallet".to_string(),
            fee_account: "the_fee_man".to_string(),
            fee_amount: Decimal::from_ratio(10_u128, 100_u128), //10%
            max_fee_amount: Decimal::from_ratio(20_u128, 100_u128), //20%

            epoch_period: 259200,   // 3 * 24 * 60 * 60 = 3 days
            unbond_period: 1814400, // 21 * 24 * 60 * 60 = 21 days
            validators: vec!["alice".to_string(), "bob".to_string(), "charlie".to_string()],
            token_factory: "CosmWasm".to_string(),
            dust_collector: Some("dusty_1".to_string()),
        },
    )
    .unwrap();

    assert_eq!(res.messages.len(), 1);
    let c = <MsgCreateDenom as Into<CosmosMsg>>::into(MsgCreateDenom {
        sender: "cosmos2contract".to_string(),
        subdenom: "boneXYZ".to_string(),
    });
    assert_eq!(res.messages[0], SubMsg::new(c));

    deps
}

fn setup_test_fee_split() -> OwnedDeps<MockStorage, MockApi, CustomQuerier> {
    let mut deps = mock_dependencies();

    let res = instantiate(
        deps.as_mut(),
        mock_env_at_timestamp(10000),
        mock_info("deployer", &[]),
        InstantiateMsg {
            owner: "larry".to_string(),
            denom: "uxyz".to_string(),
            steak_denom: "sXYZ".to_string(),
            fee_account_type: "FeeSplit".to_string(),
            fee_account: "fee_split_contract".to_string(),
            fee_amount: Decimal::from_ratio(10_u128, 100_u128), //10%
            max_fee_amount: Decimal::from_ratio(20_u128, 100_u128), //20%
            epoch_period: 259200,                               // 3 * 24 * 60 * 60 = 3 days
            unbond_period: 1814400,                             // 21 * 24 * 60 * 60 = 21 days
            validators: vec!["alice".to_string(), "bob".to_string(), "charlie".to_string()],

            token_factory: "CosmWasm".to_string(),
            dust_collector: Some("dusty_2".to_string()),
        },
    )
    .unwrap();

    assert_eq!(res.messages.len(), 1);
    let c = <MsgCreateDenom as Into<CosmosMsg>>::into(MsgCreateDenom {
        sender: "cosmos2contract".to_string(),
        subdenom: "sXYZ".to_string(),
    });
    assert_eq!(res.messages[0], SubMsg::new(c));

    deps
}

//--------------------------------------------------------------------------------------------------
// Execution
//--------------------------------------------------------------------------------------------------

#[test]
fn proper_instantiation() {
    let deps = setup_test();

    let res: ConfigResponse = query_helper(deps.as_ref(), QueryMsg::Config {});
    assert_eq!(
        res,
        ConfigResponse {
            owner: "larry".to_string(),
            new_owner: None,
            steak_token: "factory/cosmos2contract/boneXYZ".to_string(),
            epoch_period: 259200,
            unbond_period: 1814400,
            denom: "uxyz".to_string(),
            fee_type: "Wallet".to_string(),
            fee_account: "the_fee_man".to_string(),
            fee_rate: Decimal::from_ratio(10_u128, 100_u128),
            max_fee_rate: Decimal::from_ratio(20_u128, 100_u128),
            validators: vec!["alice".to_string(), "bob".to_string(), "charlie".to_string(),],
            paused_validators: vec![],
            dust_collector: Some("dusty_1".to_string()),
            token_factory: Some("CosmWasm".to_string())
        }
    );

    let res: StateResponse = query_helper(deps.as_ref(), QueryMsg::State {});
    assert_eq!(
        res,
        StateResponse {
            total_usteak: Uint128::zero(),
            total_native: Uint128::zero(),
            exchange_rate: Decimal::one(),
            unlocked_coins: vec![],
        },
    );

    let res: PendingBatch = query_helper(deps.as_ref(), QueryMsg::PendingBatch {});
    assert_eq!(
        res,
        PendingBatch {
            id: 1,
            usteak_to_burn: Uint128::zero(),
            est_unbond_start_time: 269200, // 10,000 + 259,200
        },
    );
    let deps_fee_split = setup_test_fee_split();

    let res_fee_split: ConfigResponse = query_helper(deps_fee_split.as_ref(), QueryMsg::Config {});
    assert_eq!(
        res_fee_split,
        ConfigResponse {
            owner: "larry".to_string(),
            new_owner: None,
            steak_token: "factory/cosmos2contract/sXYZ".to_string(),
            epoch_period: 259200,
            unbond_period: 1814400,
            denom: "uxyz".to_string(),
            fee_type: "FeeSplit".to_string(),
            fee_account: "fee_split_contract".to_string(),
            fee_rate: Decimal::from_ratio(10_u128, 100_u128),
            max_fee_rate: Decimal::from_ratio(20_u128, 100_u128),
            validators: vec!["alice".to_string(), "bob".to_string(), "charlie".to_string(),],
            paused_validators: vec![],
            dust_collector: Some("dusty_2".to_string()),
            token_factory: Some("CosmWasm".to_string())
        }
    );
}

#[test]
fn bonding() {
    let mut deps = setup_test();

    // Bond when no delegation has been made
    // In this case, the full deposit simply goes to the first validator
    let res = execute(
        deps.as_mut(),
        mock_env(),
        mock_info("user_1", &[Coin::new(1_000_000, "uxyz")]),
        ExecuteMsg::Bond {
            receiver: None,
            exec_msg: None,
        },
    )
    .unwrap();

    // 3 messages. (switched to 3 so we can 'send' instead of 'transfer' minted tokens, so contract
    // will know about it 1 - delegate
    // 2 - mint token (to ourselves)
    // 3 - send/transfer it
    assert_eq!(res.messages.len(), 3);
    assert_eq!(
        res.messages[0],
        SubMsg {
            msg: Delegation::new("alice", 1_000_000, "uxyz").to_cosmos_msg(),
            gas_limit: None,
            id: REPLY_REGISTER_RECEIVED_COINS,
            reply_on: ReplyOn::Never,
        }
    );
    let mint_msg = <MsgMint as Into<CosmosMsg>>::into(MsgMint {
        sender: "cosmos2contract".to_string(),
        amount: Some(denom::Coin {
            denom: "factory/cosmos2contract/boneXYZ".to_string(),
            amount: Uint128::new(1_000_000).to_string(),
        }),
    });
    assert_eq!(
        res.messages[1],
        SubMsg {
            id: 0,
            msg: mint_msg,
            gas_limit: None,
            reply_on: ReplyOn::Never,
        }
    );

    assert_eq!(
        res.messages[2],
        SubMsg {
            id: 0,
            msg: CosmosMsg::Bank(BankMsg::Send {
                to_address: "user_1".to_string(),
                amount: vec![Coin {
                    denom: "factory/cosmos2contract/boneXYZ".to_string(),
                    amount: Uint128::new(1_000_000)
                }],
            }),
            gas_limit: None,
            reply_on: ReplyOn::Never,
        }
    );

    // Bond when there are existing delegations, and Luna:Steak exchange rate is >1
    // Previously user 1 delegated 1,000,000 uluna. We assume we have accumulated 2.5% yield at
    // 1025000 staked
    deps.querier.set_staking_delegations(&[
        Delegation::new("alice", 341667, "uxyz"),
        Delegation::new("bob", 341667, "uxyz"),
        Delegation::new("charlie", 341666, "uxyz"),
    ]);

    // Charlie has the smallest amount of delegation, so the full deposit goes to him
    let res = execute(
        deps.as_mut(),
        mock_env(),
        mock_info("user_2", &[Coin::new(12345, "uxyz")]),
        ExecuteMsg::Bond {
            receiver: Some("user_3".to_string()),
            exec_msg: None,
        },
    )
    .unwrap();

    assert_eq!(res.messages.len(), 3);
    assert_eq!(
        res.messages[0],
        SubMsg {
            msg: Delegation::new("charlie", 12345, "uxyz").to_cosmos_msg(),
            id: REPLY_REGISTER_RECEIVED_COINS,
            gas_limit: None,
            reply_on: ReplyOn::Never
        }
    );
    let mint_msg = <MsgMint as Into<CosmosMsg>>::into(MsgMint {
        sender: "cosmos2contract".to_string(),
        amount: Some(denom::Coin {
            denom: "factory/cosmos2contract/boneXYZ".to_string(),
            amount: Uint128::new(12_043).to_string(),
        }),
    });
    assert_eq!(
        res.messages[1],
        SubMsg {
            id: 0,
            msg: mint_msg,
            gas_limit: None,
            reply_on: ReplyOn::Never,
        }
    );

    assert_eq!(
        res.messages[2],
        SubMsg {
            id: 0,
            msg: CosmosMsg::Bank(BankMsg::Send {
                to_address: "user_3".to_string(),

                amount: vec![Coin {
                    denom: "factory/cosmos2contract/boneXYZ".to_string(),
                    amount: Uint128::new(12_043)
                }],
            }),
            gas_limit: None,
            reply_on: ReplyOn::Never,
        }
    );

    // Check the state after bonding
    deps.querier.set_staking_delegations(&[
        Delegation::new("alice", 341667, "uxyz"),
        Delegation::new("bob", 341667, "uxyz"),
        Delegation::new("charlie", 354011, "uxyz"),
    ]);

    let res: StateResponse = query_helper(deps.as_ref(), QueryMsg::State {});
    assert_eq!(
        res,
        StateResponse {
            total_usteak: Uint128::new(1012043),
            total_native: Uint128::new(1037345),
            exchange_rate: Decimal::from_ratio(1037345u128, 1012043u128),
            unlocked_coins: vec![],
        }
    );
}

#[test]
fn harvesting() {
    let mut deps = setup_test();

    // Assume users have bonded a total of 1,000,000 uluna and minted the same amount of usteak
    deps.querier.set_staking_delegations(&[
        Delegation::new("alice", 341667, "uxyz"),
        Delegation::new("bob", 341667, "uxyz"),
        Delegation::new("charlie", 341666, "uxyz"),
    ]);

    let res = execute(deps.as_mut(), mock_env(), mock_info("worker", &[]), ExecuteMsg::Harvest {})
        .unwrap();

    assert_eq!(res.messages.len(), 4);
    assert_eq!(
        res.messages[0],
        SubMsg {
            msg: CosmosMsg::Distribution(DistributionMsg::WithdrawDelegatorReward {
                validator: "alice".to_string(),
            }),
            gas_limit: None,
            id: REPLY_REGISTER_RECEIVED_COINS,
            reply_on: ReplyOn::Never,
        }
    );
    assert_eq!(
        res.messages[1],
        SubMsg {
            msg: CosmosMsg::Distribution(DistributionMsg::WithdrawDelegatorReward {
                validator: "bob".to_string(),
            }),
            id: REPLY_REGISTER_RECEIVED_COINS,
            gas_limit: None,
            reply_on: ReplyOn::Never
        }
    );
    assert_eq!(
        res.messages[2],
        SubMsg {
            msg: CosmosMsg::Distribution(DistributionMsg::WithdrawDelegatorReward {
                validator: "charlie".to_string(),
            }),
            id: REPLY_REGISTER_RECEIVED_COINS,
            gas_limit: None,
            reply_on: ReplyOn::Never
        }
    );
    assert_eq!(
        res.messages[3],
        SubMsg {
            id: 0,
            msg: CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: MOCK_CONTRACT_ADDR.to_string(),
                msg: to_json_binary(&ExecuteMsg::Callback(CallbackMsg::Reinvest {})).unwrap(),
                funds: vec![],
            }),
            gas_limit: None,
            reply_on: ReplyOn::Never,
        }
    );
}

#[test]
fn reinvesting() {
    let mut deps = setup_test();
    let state = State::default();

    deps.querier.set_staking_delegations(&[
        Delegation::new("alice", 333334, "uxyz"),
        Delegation::new("bob", 333333, "uxyz"),
        Delegation::new("charlie", 333333, "uxyz"),
    ]);
    state.prev_denom.save(deps.as_mut().storage, &Uint128::from(0 as u32)).unwrap();
    deps.querier.set_bank_balances(&[Coin::new(234u128, "uxyz")]);

    // After the swaps, `unlocked_coins` should contain only uxyz and unknown denoms
    state
        .unlocked_coins
        .save(
            deps.as_mut().storage,
            &vec![
                Coin::new(234, "uxyz"),
                Coin::new(
                    69420,
                    "ibc/0471F1C4E7AFD3F07702BEF6DC365268D64570F7C1FDC98EA6098DD6DE59817B",
                ),
            ],
        )
        .unwrap();

    // Bob has the smallest amount of delegations, so all proceeds go to him
    let res = execute(
        deps.as_mut(),
        mock_env(),
        mock_info(MOCK_CONTRACT_ADDR, &[]),
        ExecuteMsg::Callback(CallbackMsg::Reinvest {}),
    )
    .unwrap();

    assert_eq!(res.messages.len(), 2);
    assert_eq!(
        res.messages[0],
        SubMsg {
            id: 0,
            msg: Delegation::new("bob", 234 - 23, "uxyz").to_cosmos_msg(),
            gas_limit: None,
            reply_on: ReplyOn::Never,
        }
    );
    let send_msg = BankMsg::Send {
        to_address: "the_fee_man".into(),
        amount: vec![Coin::new(23u128, "uxyz")],
    };
    assert_eq!(
        res.messages[1],
        SubMsg {
            id: 0,
            msg: CosmosMsg::Bank(send_msg),
            gas_limit: None,
            reply_on: ReplyOn::Never,
        }
    );

    // Storage should have been updated
    let unlocked_coins = state.unlocked_coins.load(deps.as_ref().storage).unwrap();
    assert_eq!(
        unlocked_coins,
        vec![Coin::new(
            69420,
            "ibc/0471F1C4E7AFD3F07702BEF6DC365268D64570F7C1FDC98EA6098DD6DE59817B",
        )],
    );
}

#[test]
fn reinvesting_fee_split() {
    let mut deps = setup_test_fee_split();
    let state = State::default();

    deps.querier.set_staking_delegations(&[
        Delegation::new("alice", 333334, "uxyz"),
        Delegation::new("bob", 333333, "uxyz"),
        Delegation::new("charlie", 333333, "uxyz"),
    ]);
    state.prev_denom.save(deps.as_mut().storage, &Uint128::from(0 as u32)).unwrap();
    deps.querier.set_bank_balances(&[Coin::new(234u128, "uxyz")]);

    // After the swaps, `unlocked_coins` should contain only uxyz and unknown denoms
    state
        .unlocked_coins
        .save(
            deps.as_mut().storage,
            &vec![
                Coin::new(234, "uxyz"),
                Coin::new(
                    69420,
                    "ibc/0471F1C4E7AFD3F07702BEF6DC365268D64570F7C1FDC98EA6098DD6DE59817B",
                ),
            ],
        )
        .unwrap();

    // Bob has the smallest amount of delegations, so all proceeds go to him
    let res = execute(
        deps.as_mut(),
        mock_env(),
        mock_info(MOCK_CONTRACT_ADDR, &[]),
        ExecuteMsg::Callback(CallbackMsg::Reinvest {}),
    )
    .unwrap();

    assert_eq!(res.messages.len(), 2);
    assert_eq!(
        res.messages[0],
        SubMsg {
            id: 0,
            msg: Delegation::new("bob", 234 - 23, "uxyz").to_cosmos_msg(),
            gas_limit: None,
            reply_on: ReplyOn::Never,
        }
    );
    let send_msg = pfc_fee_split::fee_split_msg::ExecuteMsg::Deposit {
        flush: false,
    };

    assert_eq!(
        res.messages[1],
        SubMsg {
            id: 0,
            msg: send_msg
                .into_cosmos_msg("fee_split_contract", vec![Coin::new(23u128, "uxyz")])
                .unwrap(),
            gas_limit: None,
            reply_on: ReplyOn::Never,
        }
    );

    // Storage should have been updated
    let unlocked_coins = state.unlocked_coins.load(deps.as_ref().storage).unwrap();
    assert_eq!(
        unlocked_coins,
        vec![Coin::new(
            69420,
            "ibc/0471F1C4E7AFD3F07702BEF6DC365268D64570F7C1FDC98EA6098DD6DE59817B",
        )],
    );
}

#[test]
fn queuing_unbond() {
    let mut deps = setup_test();
    let state = State::default();

    // Only Steak token is accepted for unbonding requests
    let err = execute(
        deps.as_mut(),
        mock_env(),
        mock_info(
            "random_token",
            &[Coin {
                denom: "asldkj".into(),
                amount: Uint128::new(69_420),
            }],
        ),
        ExecuteMsg::Unbond {
            receiver: None,
        },
    )
    .unwrap_err();

    assert_eq!(
        err,
        StdError::generic_err("you can only send factory/cosmos2contract/boneXYZ tokens to unbond")
    );

    // User 1 creates an unbonding request before `est_unbond_start_time` is reached. The unbond
    // request is saved, but not the pending batch is not submitted for unbonding
    let res = execute(
        deps.as_mut(),
        mock_env_at_timestamp(12345), // est_unbond_start_time = 269200
        mock_info(
            "steak_token",
            &[Coin {
                denom: "factory/cosmos2contract/boneXYZ".into(),
                amount: Uint128::new(23_456),
            }],
        ),
        ExecuteMsg::Unbond {
            receiver: Some("user_1".to_string()),
        },
    )
    .unwrap();

    assert_eq!(res.messages.len(), 0);

    // User 2 creates an unbonding request after `est_unbond_start_time` is reached. The unbond
    // request is saved, and the pending is automatically submitted for unbonding
    let res = execute(
        deps.as_mut(),
        mock_env_at_timestamp(269201), // est_unbond_start_time = 269200
        mock_info(
            "user_3",
            &[Coin {
                denom: "factory/cosmos2contract/boneXYZ".into(),
                amount: Uint128::new(69_420),
            }],
        ),
        ExecuteMsg::Unbond {
            receiver: None,
        },
    )
    .unwrap();

    assert_eq!(res.messages.len(), 1);
    assert_eq!(
        res.messages[0],
        SubMsg {
            id: 0,
            msg: CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: MOCK_CONTRACT_ADDR.to_string(),
                msg: to_json_binary(&ExecuteMsg::SubmitBatch {}).unwrap(),
                funds: vec![],
            }),
            gas_limit: None,
            reply_on: ReplyOn::Never,
        }
    );

    // The users' unbonding requests should have been saved
    let ubr1 = unbond_requests()
        .load(deps.as_ref().storage, (1u64.into(), &Addr::unchecked("user_1").to_string()))
        .unwrap();
    let ubr2 = unbond_requests()
        .load(deps.as_ref().storage, (1u64.into(), &Addr::unchecked("user_3").to_string()))
        .unwrap();

    assert_eq!(
        ubr1,
        UnbondRequest {
            id: 1,
            user: Addr::unchecked("user_1"),
            shares: Uint128::new(23456),
        }
    );
    assert_eq!(
        ubr2,
        UnbondRequest {
            id: 1,
            user: Addr::unchecked("user_3"),
            shares: Uint128::new(69420),
        }
    );

    // Pending batch should have been updated
    let pending_batch = state.pending_batch.load(deps.as_ref().storage).unwrap();
    assert_eq!(
        pending_batch,
        PendingBatch {
            id: 1,
            usteak_to_burn: Uint128::new(92876), // 23,456 + 69,420
            est_unbond_start_time: 269200,
        }
    );
}

#[test]
fn submitting_batch() {
    let mut deps = setup_test();
    let state = State::default();

    // uluna bonded: 1,037,345
    // usteak supply: 1,012,043
    // uluna per ustake: 1.025
    deps.querier.set_staking_delegations(&[
        Delegation::new("alice", 345_782, "uxyz"),
        Delegation::new("bob", 345_782, "uxyz"),
        Delegation::new("charlie", 345_781, "uxyz"),
    ]);
    state.steak_minted.save(deps.as_mut().storage, &Uint128::new(1_012_043)).unwrap();

    // We continue from the contract state at the end of the last test
    let unbond_reqs = vec![
        UnbondRequest {
            id: 1,
            user: Addr::unchecked("user_1"),
            shares: Uint128::new(23456),
        },
        UnbondRequest {
            id: 1,
            user: Addr::unchecked("user_3"),
            shares: Uint128::new(69420),
        },
    ];

    for unbond_request in &unbond_reqs {
        unbond_requests()
            .save(
                deps.as_mut().storage,
                (unbond_request.id.into(), &Addr::unchecked(unbond_request.user.clone()).as_str()),
                unbond_request,
            )
            .unwrap();
    }

    state
        .pending_batch
        .save(
            deps.as_mut().storage,
            &PendingBatch {
                id: 1,
                usteak_to_burn: Uint128::new(92876), // 23,456 + 69,420
                est_unbond_start_time: 269200,
            },
        )
        .unwrap();

    // Anyone can invoke `submit_batch`. Here we continue from the previous test and assume it is
    // invoked automatically as user 2 submits the unbonding request
    //
    // usteak to burn: 23,456 + 69,420 = 92,876
    // uluna to unbond: 1,037,345 * 92,876 / 1,012,043 = 95,197
    //
    // Target: (1,037,345 - 95,197) / 3 = 314,049
    // Remainer: 1
    // Alice:   345,782 - (314,049 + 1) = 31,732
    // Bob:     345,782 - (314,049 + 0) = 31,733
    // Charlie: 345,781 - (314,049 + 0) = 31,732
    let res = execute(
        deps.as_mut(),
        mock_env_at_timestamp(269_201),
        mock_info(MOCK_CONTRACT_ADDR, &[]),
        ExecuteMsg::SubmitBatch {},
    )
    .unwrap();

    assert_eq!(res.messages.len(), 4);
    assert_eq!(
        res.messages[0],
        SubMsg {
            msg: Undelegation::new("alice", 31732, "uxyz").to_cosmos_msg(),
            id: REPLY_REGISTER_RECEIVED_COINS,
            gas_limit: None,
            reply_on: ReplyOn::Never
        }
    );
    assert_eq!(
        res.messages[1],
        SubMsg {
            msg: Undelegation::new("bob", 31733, "uxyz").to_cosmos_msg(),
            id: REPLY_REGISTER_RECEIVED_COINS,
            gas_limit: None,
            reply_on: ReplyOn::Never
        }
    );
    assert_eq!(
        res.messages[2],
        SubMsg {
            msg: Undelegation::new("charlie", 31732, "uxyz").to_cosmos_msg(),
            id: REPLY_REGISTER_RECEIVED_COINS,
            gas_limit: None,
            reply_on: ReplyOn::Never
        }
    );

    let burn_msg = <MsgBurn as Into<CosmosMsg>>::into(MsgBurn {
        sender: "cosmos2contract".to_string(),
        amount: Some(denom::Coin {
            denom: "factory/cosmos2contract/boneXYZ".to_string(),
            amount: Uint128::new(92_876).to_string(),
        }),
    });
    assert_eq!(
        res.messages[3],
        SubMsg {
            id: 0,
            msg: burn_msg,
            gas_limit: None,
            reply_on: ReplyOn::Never,
        }
    );

    // A new pending batch should have been created
    let pending_batch = state.pending_batch.load(deps.as_ref().storage).unwrap();
    assert_eq!(
        pending_batch,
        PendingBatch {
            id: 2,
            usteak_to_burn: Uint128::zero(),
            est_unbond_start_time: 528401, // 269,201 + 259,200
        }
    );

    // Previous batch should have been updated
    let previous_batch = previous_batches().load(deps.as_ref().storage, 1u64.into()).unwrap();
    assert_eq!(
        previous_batch,
        Batch {
            id: 1,
            reconciled: false,
            total_shares: Uint128::new(92876),
            amount_unclaimed: Uint128::new(95197),
            est_unbond_end_time: 2083601, // 269,201 + 1,814,400
        }
    );
}

#[test]
fn reconciling() {
    let mut deps = setup_test();
    let state = State::default();

    let previous_batch_list = vec![
        Batch {
            id: 1,
            reconciled: true,
            total_shares: Uint128::new(92876),
            amount_unclaimed: Uint128::new(95197), // 1.025 Luna per Steak
            est_unbond_end_time: 10000,
        },
        Batch {
            id: 2,
            reconciled: false,
            total_shares: Uint128::new(1345),
            amount_unclaimed: Uint128::new(1385), // 1.030 Luna per Steak
            est_unbond_end_time: 20000,
        },
        Batch {
            id: 3,
            reconciled: false,
            total_shares: Uint128::new(1456),
            amount_unclaimed: Uint128::new(1506), // 1.035 Luna per Steak
            est_unbond_end_time: 30000,
        },
        Batch {
            id: 4,
            reconciled: false,
            total_shares: Uint128::new(1567),
            amount_unclaimed: Uint128::new(1629), // 1.040 Luna per Steak
            est_unbond_end_time: 40000,           // not yet finished unbonding, ignored
        },
    ];

    for previous_batch in &previous_batch_list {
        previous_batches()
            .save(deps.as_mut().storage, previous_batch.id.into(), previous_batch)
            .unwrap();
    }

    state
        .unlocked_coins
        .save(
            deps.as_mut().storage,
            &vec![
                Coin::new(10000, "uxyz"),
                Coin::new(234, "ukrw"),
                Coin::new(345, "uusd"),
                Coin::new(
                    69420,
                    "ibc/0471F1C4E7AFD3F07702BEF6DC365268D64570F7C1FDC98EA6098DD6DE59817B",
                ),
            ],
        )
        .unwrap();

    deps.querier.set_bank_balances(&[
        Coin::new(12345, "uxyz"),
        Coin::new(234, "ukrw"),
        Coin::new(345, "uusd"),
        Coin::new(69420, "ibc/0471F1C4E7AFD3F07702BEF6DC365268D64570F7C1FDC98EA6098DD6DE59817B"),
    ]);

    execute(
        deps.as_mut(),
        mock_env_at_timestamp(35000),
        mock_info("worker", &[]),
        ExecuteMsg::Reconcile {},
    )
    .unwrap();

    // Expected received: batch 2 + batch 3 = 1385 + 1506 = 2891
    // Expected unlocked: 10000
    // Expected: 12891
    // Actual: 12345
    // Shortfall: 12891 - 12345 = 456
    //
    // uluna per batch: 546 / 2 = 273
    // remainder: 0
    // batch 2: 1385 - 273 = 1112
    // batch 3: 1506 - 273 = 1233
    let batch = previous_batches().load(deps.as_ref().storage, 2u64.into()).unwrap();
    assert_eq!(
        batch,
        Batch {
            id: 2,
            reconciled: true,
            total_shares: Uint128::new(1345),
            amount_unclaimed: Uint128::new(1112), // 1385 - 273
            est_unbond_end_time: 20000,
        }
    );

    let batch = previous_batches().load(deps.as_ref().storage, 3u64.into()).unwrap();
    assert_eq!(
        batch,
        Batch {
            id: 3,
            reconciled: true,
            total_shares: Uint128::new(1456),
            amount_unclaimed: Uint128::new(1233), // 1506 - 273
            est_unbond_end_time: 30000,
        }
    );

    // Batches 1 and 4 should not have changed
    let batch = previous_batches().load(deps.as_ref().storage, 1u64.into()).unwrap();
    assert_eq!(batch, previous_batch_list[0]);

    let batch = previous_batches().load(deps.as_ref().storage, 4u64.into()).unwrap();
    assert_eq!(batch, previous_batch_list[3]);
}

#[test]
fn withdrawing_unbonded() {
    let mut deps = setup_test();
    let state = State::default();

    // We simulate a most general case:
    // - batches 1 and 2 have finished unbonding
    // - batch 3 have been submitted for unbonding but have not finished
    // - batch 4 is still pending
    let unbond_reqs = vec![
        UnbondRequest {
            id: 1,
            user: Addr::unchecked("user_1"),
            shares: Uint128::new(23456),
        },
        UnbondRequest {
            id: 1,
            user: Addr::unchecked("user_3"),
            shares: Uint128::new(69420),
        },
        UnbondRequest {
            id: 2,
            user: Addr::unchecked("user_1"),
            shares: Uint128::new(34567),
        },
        UnbondRequest {
            id: 3,
            user: Addr::unchecked("user_1"),
            shares: Uint128::new(45678),
        },
        UnbondRequest {
            id: 4,
            user: Addr::unchecked("user_1"),
            shares: Uint128::new(56789),
        },
    ];

    for unbond_request in &unbond_reqs {
        unbond_requests()
            .save(
                deps.as_mut().storage,
                (unbond_request.id.into(), &Addr::unchecked(unbond_request.user.clone()).as_str()),
                unbond_request,
            )
            .unwrap();
    }

    let previous_batch_list = vec![
        Batch {
            id: 1,
            reconciled: true,
            total_shares: Uint128::new(92876),
            amount_unclaimed: Uint128::new(95197), // 1.025 Luna per Steak
            est_unbond_end_time: 10000,
        },
        Batch {
            id: 2,
            reconciled: true,
            total_shares: Uint128::new(34567),
            amount_unclaimed: Uint128::new(35604), // 1.030 Luna per Steak
            est_unbond_end_time: 20000,
        },
        Batch {
            id: 3,
            reconciled: false, // finished unbonding, but not reconciled; ignored
            total_shares: Uint128::new(45678),
            amount_unclaimed: Uint128::new(47276), // 1.035 Luna per Steak
            est_unbond_end_time: 20000,
        },
        Batch {
            id: 4,
            reconciled: true,
            total_shares: Uint128::new(56789),
            amount_unclaimed: Uint128::new(59060), // 1.040 Luna per Steak
            est_unbond_end_time: 30000,            /* reconciled, but not yet finished unbonding;
                                                    * ignored */
        },
    ];

    for previous_batch in &previous_batch_list {
        previous_batches()
            .save(deps.as_mut().storage, previous_batch.id.into(), previous_batch)
            .unwrap();
    }

    state
        .pending_batch
        .save(
            deps.as_mut().storage,
            &PendingBatch {
                id: 4,
                usteak_to_burn: Uint128::new(56789),
                est_unbond_start_time: 100000,
            },
        )
        .unwrap();

    // Attempt to withdraw before any batch has completed unbonding. Should error
    let err = execute(
        deps.as_mut(),
        mock_env_at_timestamp(5000),
        mock_info("user_1", &[]),
        ExecuteMsg::WithdrawUnbonded {
            receiver: None,
        },
    )
    .unwrap_err();

    assert_eq!(err, StdError::generic_err("withdrawable amount is zero"));

    // Attempt to withdraw once batches 1 and 2 have finished unbonding, but 3 has not yet
    //
    // Withdrawable from batch 1: 95,197 * 23,456 / 92,876 = 24,042
    // Withdrawable from batch 2: 35,604
    // Total withdrawable: 24,042 + 35,604 = 59,646
    //
    // Batch 1 should be updated:
    // Total shares: 92,876 - 23,456 = 69,420
    // Unclaimed uluna: 95,197 - 24,042 = 71,155
    //
    // Batch 2 is completely withdrawn, should be purged from storage
    let res = execute(
        deps.as_mut(),
        mock_env_at_timestamp(25000),
        mock_info("user_1", &[]),
        ExecuteMsg::WithdrawUnbonded {
            receiver: None,
        },
    )
    .unwrap();

    assert_eq!(res.messages.len(), 1);
    assert_eq!(
        res.messages[0],
        SubMsg {
            id: 0,
            msg: CosmosMsg::Bank(BankMsg::Send {
                to_address: "user_1".to_string(),
                amount: vec![Coin::new(59646, "uxyz")],
            }),
            gas_limit: None,
            reply_on: ReplyOn::Never,
        }
    );

    // Previous batches should have been updated
    let batch = previous_batches().load(deps.as_ref().storage, 1u64.into()).unwrap();
    assert_eq!(
        batch,
        Batch {
            id: 1,
            reconciled: true,
            total_shares: Uint128::new(69420),
            amount_unclaimed: Uint128::new(71155),
            est_unbond_end_time: 10000,
        }
    );

    let err = previous_batches().load(deps.as_ref().storage, 2u64.into()).unwrap_err();
    match err { StdError::NotFound {..} => {}, _=> {
        panic!("Should have been not found")
    } };

    

    // User 1's unbond requests in batches 1 and 2 should have been deleted
    let err1 = unbond_requests()
        .load(deps.as_ref().storage, (1u64.into(), &Addr::unchecked("user_1").as_str()))
        .unwrap_err();
    let err2 = unbond_requests()
        .load(deps.as_ref().storage, (1u64.into(), &Addr::unchecked("user_1").as_str()))
        .unwrap_err();

    match err1 { StdError::NotFound {..} => {}, _=> {
        panic!("Should have been not found")
    } };

    match err2 { StdError::NotFound {..} => {}, _=> {
        panic!("Should have been not found")
    } };


    // User 3 attempt to withdraw; also specifying a receiver
    let res = execute(
        deps.as_mut(),
        mock_env_at_timestamp(25000),
        mock_info("user_3", &[]),
        ExecuteMsg::WithdrawUnbonded {
            receiver: Some("user_2".to_string()),
        },
    )
    .unwrap();

    assert_eq!(res.messages.len(), 1);
    assert_eq!(
        res.messages[0],
        SubMsg {
            id: 0,
            msg: CosmosMsg::Bank(BankMsg::Send {
                to_address: "user_2".to_string(),
                amount: vec![Coin::new(71155, "uxyz")],
            }),
            gas_limit: None,
            reply_on: ReplyOn::Never,
        }
    );

    // Batch 1 and user 2's unbonding request should have been purged from storage
    let err = previous_batches().load(deps.as_ref().storage, 1u64.into()).unwrap_err();
    match err { StdError::NotFound {..} => {}, _=> {
        panic!("Should have been not found")
    } };


    let err = unbond_requests()
        .load(deps.as_ref().storage, (1u64.into(), &Addr::unchecked("user_3").as_str()))
        .unwrap_err();

    match err { StdError::NotFound {..} => {}, _=> {
        panic!("Should have been not found")
    } };

}

#[test]
fn adding_validator() {
    let mut deps = setup_test();
    //let state = State::default();

    let err = execute(
        deps.as_mut(),
        mock_env(),
        mock_info("jake", &[]),
        ExecuteMsg::AddValidator {
            validator: "dave".to_string(),
        },
    )
    .unwrap_err();

    assert_eq!(err, StdError::generic_err("unauthorized: sender is not owner"));

    let err = execute(
        deps.as_mut(),
        mock_env(),
        mock_info("larry", &[]),
        ExecuteMsg::AddValidator {
            validator: "alice".to_string(),
        },
    )
    .unwrap_err();

    assert_eq!(err, StdError::generic_err("validator is already whitelisted"));

    let res = execute(
        deps.as_mut(),
        mock_env(),
        mock_info("larry", &[]),
        ExecuteMsg::AddValidator {
            validator: "dave".to_string(),
        },
    )
    .unwrap();

    assert_eq!(res.messages.len(), 0);

    assert!(VALIDATORS.contains(deps.as_ref().storage, "alice"));
    assert!(VALIDATORS.contains(deps.as_ref().storage, "bob"));
    assert!(VALIDATORS.contains(deps.as_ref().storage, "charlie"));
    assert!(VALIDATORS.contains(deps.as_ref().storage, "dave"));
    assert_eq!(VALIDATORS.count(deps.as_ref().storage).unwrap(), 4);
}

#[test]
fn removing_validator() {
    let mut deps = setup_test();
    //let state = State::default();

    deps.querier.set_staking_delegations(&[
        Delegation::new("alice", 341667, "uxyz"),
        Delegation::new("bob", 341667, "uxyz"),
        Delegation::new("charlie", 341666, "uxyz"),
    ]);

    let err = execute(
        deps.as_mut(),
        mock_env(),
        mock_info("jake", &[]),
        ExecuteMsg::RemoveValidator {
            validator: "charlie".to_string(),
        },
    )
    .unwrap_err();

    assert_eq!(err, StdError::generic_err("unauthorized: sender is not owner"));

    let err = execute(
        deps.as_mut(),
        mock_env(),
        mock_info("larry", &[]),
        ExecuteMsg::RemoveValidator {
            validator: "dave".to_string(),
        },
    )
    .unwrap_err();

    assert_eq!(err, StdError::generic_err("validator is not already whitelisted"));

    // Target: (341667 + 341667 + 341666) / 2 = 512500
    // Remainder: 0
    // Alice:   512500 + 0 - 341667 = 170833
    // Bob:     512500 + 0 - 341667 = 170833
    let res = execute(
        deps.as_mut(),
        mock_env(),
        mock_info("larry", &[]),
        ExecuteMsg::RemoveValidator {
            validator: "charlie".to_string(),
        },
    )
    .unwrap();

    assert_eq!(res.messages.len(), 2);
    assert_eq!(
        res.messages[0],
        SubMsg {
            msg: Redelegation::new("charlie", "alice", 170833, "uxyz").to_cosmos_msg(),
            id: REPLY_REGISTER_RECEIVED_COINS,
            gas_limit: None,
            reply_on: ReplyOn::Never
        },
    );
    assert_eq!(
        res.messages[1],
        SubMsg {
            msg: Redelegation::new("charlie", "bob", 170833, "uxyz").to_cosmos_msg(),
            id: REPLY_REGISTER_RECEIVED_COINS,
            gas_limit: None,
            reply_on: ReplyOn::Never
        },
    );

    assert!(VALIDATORS.contains(deps.as_ref().storage, "alice"));
    assert!(VALIDATORS.contains(deps.as_ref().storage, "bob"));
    assert_eq!(VALIDATORS.count(deps.as_ref().storage).unwrap(), 2);
}

#[test]
fn transferring_ownership() {
    let mut deps = setup_test();
    let state = State::default();

    let err = execute(
        deps.as_mut(),
        mock_env(),
        mock_info("jake", &[]),
        ExecuteMsg::TransferOwnership {
            new_owner: "jake".to_string(),
        },
    )
    .unwrap_err();

    assert_eq!(err, StdError::generic_err("unauthorized: sender is not owner"));

    let res = execute(
        deps.as_mut(),
        mock_env(),
        mock_info("larry", &[]),
        ExecuteMsg::TransferOwnership {
            new_owner: "jake".to_string(),
        },
    )
    .unwrap();

    assert_eq!(res.messages.len(), 0);

    let owner = state.owner.load(deps.as_ref().storage).unwrap();
    assert_eq!(owner, Addr::unchecked("larry"));

    let err = execute(
        deps.as_mut(),
        mock_env(),
        mock_info("pumpkin", &[]),
        ExecuteMsg::AcceptOwnership {},
    )
    .unwrap_err();

    assert_eq!(err, StdError::generic_err("unauthorized: sender is not new owner"));

    let res =
        execute(deps.as_mut(), mock_env(), mock_info("jake", &[]), ExecuteMsg::AcceptOwnership {})
            .unwrap();

    assert_eq!(res.messages.len(), 0);

    let owner = state.owner.load(deps.as_ref().storage).unwrap();
    assert_eq!(owner, Addr::unchecked("jake"));
}

#[test]
fn splitting_fees() {
    let mut deps = setup_test();

    let err = execute(
        deps.as_mut(),
        mock_env(),
        mock_info("jake", &[]),
        ExecuteMsg::TransferFeeAccount {
            fee_account_type: "Wallet".to_string(),
            new_fee_account: "charlie".to_string(),
        },
    )
    .unwrap_err();

    assert_eq!(err, StdError::generic_err("unauthorized: sender is not owner"));

    let err = execute(
        deps.as_mut(),
        mock_env(),
        mock_info("larry", &[]),
        ExecuteMsg::TransferFeeAccount {
            fee_account_type: "xxxx".to_string(),
            new_fee_account: "charlie".to_string(),
        },
    )
    .unwrap_err();

    assert_eq!(err, StdError::generic_err("Invalid Fee type: Wallet or FeeSplit only"));

    execute(
        deps.as_mut(),
        mock_env(),
        mock_info("larry", &[]),
        ExecuteMsg::TransferFeeAccount {
            fee_account_type: "Wallet".to_string(),
            new_fee_account: "charlie".to_string(),
        },
    )
    .unwrap();
    let res: ConfigResponse = query_helper(deps.as_ref(), QueryMsg::Config {});
    assert_eq!(
        res,
        ConfigResponse {
            owner: "larry".to_string(),
            new_owner: None,
            steak_token: "factory/cosmos2contract/boneXYZ".to_string(),
            epoch_period: 259200,
            unbond_period: 1814400,
            denom: "uxyz".to_string(),
            fee_type: "Wallet".to_string(),
            fee_account: "charlie".to_string(),
            fee_rate: Decimal::from_ratio(10_u128, 100_u128),
            max_fee_rate: Decimal::from_ratio(20_u128, 100_u128),
            validators: vec!["alice".to_string(), "bob".to_string(), "charlie".to_string(),],
            paused_validators: vec![],
            dust_collector: Some("dusty_1".to_string()),
            token_factory: Some("CosmWasm".to_string())
        }
    );

    execute(
        deps.as_mut(),
        mock_env(),
        mock_info("larry", &[]),
        ExecuteMsg::TransferFeeAccount {
            fee_account_type: "FeeSplit".to_string(),
            new_fee_account: "contract".to_string(),
        },
    )
    .unwrap();
    let res: ConfigResponse = query_helper(deps.as_ref(), QueryMsg::Config {});
    assert_eq!(
        res,
        ConfigResponse {
            owner: "larry".to_string(),
            new_owner: None,
            steak_token: "factory/cosmos2contract/boneXYZ".to_string(),
            epoch_period: 259200,
            unbond_period: 1814400,
            denom: "uxyz".to_string(),
            fee_type: "FeeSplit".to_string(),
            fee_account: "contract".to_string(),
            fee_rate: Decimal::from_ratio(10_u128, 100_u128),
            max_fee_rate: Decimal::from_ratio(20_u128, 100_u128),
            validators: vec!["alice".to_string(), "bob".to_string(), "charlie".to_string(),],
            paused_validators: vec![],
            dust_collector: Some("dusty_1".to_string()),
            token_factory: Some("CosmWasm".to_string())
        }
    );
}
//--------------------------------------------------------------------------------------------------
// Queries
//--------------------------------------------------------------------------------------------------

#[test]
fn querying_previous_batches() {
    let mut deps = mock_dependencies();

    let batches = vec![
        Batch {
            id: 1,
            reconciled: false,
            total_shares: Uint128::new(123),
            amount_unclaimed: Uint128::new(678),
            est_unbond_end_time: 10000,
        },
        Batch {
            id: 2,
            reconciled: true,
            total_shares: Uint128::new(234),
            amount_unclaimed: Uint128::new(789),
            est_unbond_end_time: 15000,
        },
        Batch {
            id: 3,
            reconciled: false,
            total_shares: Uint128::new(345),
            amount_unclaimed: Uint128::new(890),
            est_unbond_end_time: 20000,
        },
        Batch {
            id: 4,
            reconciled: true,
            total_shares: Uint128::new(456),
            amount_unclaimed: Uint128::new(999),
            est_unbond_end_time: 25000,
        },
    ];

    //let state = State::default();
    for batch in &batches {
        previous_batches().save(deps.as_mut().storage, batch.id.into(), batch).unwrap();
    }

    // Querying a single batch
    let res: Batch = query_helper(deps.as_ref(), QueryMsg::PreviousBatch(1));
    assert_eq!(res, batches[0].clone());

    let res: Batch = query_helper(deps.as_ref(), QueryMsg::PreviousBatch(2));
    assert_eq!(res, batches[1].clone());

    // Query multiple batches
    let res: Vec<Batch> = query_helper(
        deps.as_ref(),
        QueryMsg::PreviousBatches {
            start_after: None,
            limit: None,
        },
    );
    assert_eq!(res, batches.clone());

    let res: Vec<Batch> = query_helper(
        deps.as_ref(),
        QueryMsg::PreviousBatches {
            start_after: Some(1),
            limit: None,
        },
    );
    assert_eq!(res, vec![batches[1].clone(), batches[2].clone(), batches[3].clone()]);

    let res: Vec<Batch> = query_helper(
        deps.as_ref(),
        QueryMsg::PreviousBatches {
            start_after: Some(4),
            limit: None,
        },
    );
    assert_eq!(res, vec![]);

    // Query multiple batches, indexed by whether it has been reconciled
    let res = previous_batches()
        .idx
        .reconciled
        .prefix("true".into())
        .range(deps.as_ref().storage, None, None, Order::Ascending)
        .map(|item| {
            let (_, v) = item.unwrap();
            v
        })
        .collect::<Vec<_>>();

    assert_eq!(res, vec![batches[1].clone(), batches[3].clone()]);

    let res = previous_batches()
        .idx
        .reconciled
        .prefix("false".into())
        .range(deps.as_ref().storage, None, None, Order::Ascending)
        .map(|item| {
            let (_, v) = item.unwrap();
            v
        })
        .collect::<Vec<_>>();

    assert_eq!(res, vec![batches[0].clone(), batches[2].clone()]);
}

#[test]
fn querying_unbond_requests() {
    let mut deps = mock_dependencies();
    //let state = State::default();

    let unbond_reqs = vec![
        UnbondRequest {
            id: 1,
            user: Addr::unchecked("alice"),
            shares: Uint128::new(123),
        },
        UnbondRequest {
            id: 1,
            user: Addr::unchecked("bob"),
            shares: Uint128::new(234),
        },
        UnbondRequest {
            id: 1,
            user: Addr::unchecked("charlie"),
            shares: Uint128::new(345),
        },
        UnbondRequest {
            id: 2,
            user: Addr::unchecked("alice"),
            shares: Uint128::new(456),
        },
    ];

    for unbond_request in &unbond_reqs {
        unbond_requests()
            .save(
                deps.as_mut().storage,
                (unbond_request.id.into(), &Addr::unchecked(unbond_request.user.clone()).as_str()),
                unbond_request,
            )
            .unwrap();
    }

    let res: Vec<UnbondRequestsByBatchResponseItem> = query_helper(
        deps.as_ref(),
        QueryMsg::UnbondRequestsByBatch {
            id: 1,
            start_after: None,
            limit: None,
        },
    );
    assert_eq!(
        res,
        vec![
            unbond_reqs[0].clone().into(),
            unbond_reqs[1].clone().into(),
            unbond_reqs[2].clone().into(),
        ]
    );

    let res: Vec<UnbondRequestsByBatchResponseItem> = query_helper(
        deps.as_ref(),
        QueryMsg::UnbondRequestsByBatch {
            id: 2,
            start_after: None,
            limit: None,
        },
    );
    assert_eq!(res, vec![unbond_reqs[3].clone().into()]);

    let res: Vec<UnbondRequestsByUserResponseItem> = query_helper(
        deps.as_ref(),
        QueryMsg::UnbondRequestsByUser {
            user: "alice".to_string(),
            start_after: None,
            limit: None,
        },
    );
    assert_eq!(res, vec![unbond_reqs[0].clone().into(), unbond_reqs[3].clone().into(),]);
    /*
        for x in unbond_requests().range(deps.as_ref().storage, None, None, Order::Ascending) {
            let rec = x.unwrap();
            eprintln!("Key {}/{} = {:?}", rec.0.0, rec.0.1, rec.1)
        }
    */
    let res: Vec<UnbondRequestsByUserResponseItem> = query_helper(
        deps.as_ref(),
        QueryMsg::UnbondRequestsByUser {
            user: "alice".to_string(),
            start_after: Some(1),
            limit: None,
        },
    );
    assert_eq!(res, vec![unbond_reqs[3].clone().into()]);
}

//--------------------------------------------------------------------------------------------------
// Delegations
//--------------------------------------------------------------------------------------------------

#[test]
fn computing_undelegations() {
    let current_delegations = vec![
        Delegation::new("alice", 400, "uxyz"),
        Delegation::new("bob", 300, "uxyz"),
        Delegation::new("charlie", 200, "uxyz"),
    ];

    // Target: (400 + 300 + 200 - 451) / 3 = 149
    // Remainder: 2
    // Alice:   400 - (149 + 1) = 250
    // Bob:     300 - (149 + 1) = 150
    // Charlie: 200 - (149 + 0) = 51
    let new_undelegations = compute_undelegations(Uint128::new(451), &current_delegations, "uxyz");
    let expected = vec![
        Undelegation::new("alice", 250, "uxyz"),
        Undelegation::new("bob", 150, "uxyz"),
        Undelegation::new("charlie", 51, "uxyz"),
    ];
    assert_eq!(new_undelegations, expected);
}

#[test]
fn computing_redelegations_for_removal() {
    let current_delegations = vec![
        Delegation::new("alice", 13000, "uxyz"),
        Delegation::new("bob", 12000, "uxyz"),
        Delegation::new("charlie", 11000, "uxyz"),
        Delegation::new("dave", 10000, "uxyz"),
    ];

    // Suppose Dave will be removed
    // uluna_per_validator = (13000 + 12000 + 11000 + 10000) / 3 = 15333
    // remainder = 1
    // to Alice:   15333 + 1 - 13000 = 2334
    // to Bob:     15333 + 0 - 12000 = 3333
    // to Charlie: 15333 + 0 - 11000 = 4333
    let expected = vec![
        Redelegation::new("dave", "alice", 2334, "uxyz"),
        Redelegation::new("dave", "bob", 3333, "uxyz"),
        Redelegation::new("dave", "charlie", 4333, "uxyz"),
    ];

    assert_eq!(
        compute_redelegations_for_removal(
            &current_delegations[3],
            &current_delegations[..3],
            "uxyz",
        ),
        expected,
    );
}

#[test]
fn computing_redelegations_for_rebalancing() {
    let current_delegations = vec![
        Delegation::new("alice", 69420, "uxyz"),
        Delegation::new("bob", 1234, "uxyz"),
        Delegation::new("charlie", 88888, "uxyz"),
        Delegation::new("dave", 40471, "uxyz"),
        Delegation::new("evan", 2345, "uxyz"),
    ];
    let active_validators: Vec<String> = vec![
        "alice".to_string(),
        "bob".to_string(),
        "charlie".to_string(),
        "dave".to_string(),
        "evan".to_string(),
    ];
    // uluna_per_validator = (69420 + 88888 + 1234 + 40471 + 2345) / 4 = 40471
    // remainer = 3
    // src_delegations:
    //  - alice:   69420 - (40471 + 1) = 28948
    //  - charlie: 88888 - (40471 + 1) = 48416
    // dst_delegations:
    //  - bob:     (40471 + 1) - 1234  = 39238
    //  - evan:    (40471 + 0) - 2345  = 38126
    //
    // Round 1: alice --(28948)--> bob
    // src_delegations:
    //  - charlie: 48416
    // dst_delegations:
    //  - bob:     39238 - 28948 = 10290
    //  - evan:    38126
    //
    // Round 2: charlie --(10290)--> bob
    // src_delegations:
    //  - charlie: 48416 - 10290 = 38126
    // dst_delegations:
    //  - evan:    38126
    //
    // Round 3: charlie --(38126)--> evan
    // Queues are emptied
    let expected = vec![
        Redelegation::new("alice", "bob", 28948, "uxyz"),
        Redelegation::new("charlie", "bob", 10290, "uxyz"),
        Redelegation::new("charlie", "evan", 38126, "uxyz"),
    ];

    assert_eq!(
        compute_redelegations_for_rebalancing(
            active_validators,
            &current_delegations,
            Uint128::from(10 as u64)
        ),
        expected,
    );

    let partially_active =
        vec!["alice".to_string(), "charlie".to_string(), "dave".to_string(), "evan".to_string()];

    let partially_expected = vec![
        Redelegation::new("alice", "dave", 10118, "uxyz"),
        Redelegation::new("alice", "evan", 8712, "uxyz"),
        Redelegation::new("charlie", "evan", 38299, "uxyz"),
    ];
    assert_eq!(
        compute_redelegations_for_rebalancing(
            partially_active.clone(),
            &current_delegations,
            Uint128::from(10 as u64)
        ),
        partially_expected,
    );

    let partially_expected_minimums = vec![
        Redelegation::new("alice", "evan", 18830, "uxyz"),
        Redelegation::new("charlie", "evan", 29414, "uxyz"),
    ];
    assert_eq!(
        compute_redelegations_for_rebalancing(
            partially_active,
            &current_delegations,
            Uint128::from(15_000 as u64)
        ),
        partially_expected_minimums,
    );
}

//--------------------------------------------------------------------------------------------------
// Coins
//--------------------------------------------------------------------------------------------------

#[test]
fn parsing_coin() {
    let coin = parse_coin("12345uatom").unwrap();
    assert_eq!(coin, Coin::new(12345, "uatom"));

    let coin =
        parse_coin("23456ibc/0471F1C4E7AFD3F07702BEF6DC365268D64570F7C1FDC98EA6098DD6DE59817B")
            .unwrap();
    assert_eq!(
        coin,
        Coin::new(23456, "ibc/0471F1C4E7AFD3F07702BEF6DC365268D64570F7C1FDC98EA6098DD6DE59817B",)
    );

    let err = parse_coin("69420").unwrap_err();
    assert_eq!(err, StdError::generic_err("failed to parse coin: 69420"));

    let err = parse_coin("ngmi").unwrap_err();
    assert_eq!(err, StdError::generic_err("Parsing u128: cannot parse integer from empty string"));
}

#[test]
fn parsing_coins() {
    let coins = Coins::from_str("").unwrap();
    assert_eq!(coins.0, vec![]);

    let coins = Coins::from_str("12345uatom").unwrap();
    assert_eq!(coins.0, vec![Coin::new(12345, "uatom")]);

    let coins = Coins::from_str("12345uatom,23456uxyz").unwrap();
    assert_eq!(coins.0, vec![Coin::new(12345, "uatom"), Coin::new(23456, "uxyz")]);
}

#[test]
fn adding_coins() {
    let mut coins = Coins(vec![]);

    coins.add(&Coin::new(12345, "uatom")).unwrap();
    assert_eq!(coins.0, vec![Coin::new(12345, "uatom")]);

    coins.add(&Coin::new(23456, "uxyz")).unwrap();
    assert_eq!(coins.0, vec![Coin::new(12345, "uatom"), Coin::new(23456, "uxyz")]);

    coins.add_many(&Coins::from_str("76543uatom,69420uusd").unwrap()).unwrap();
    assert_eq!(
        coins.0,
        vec![Coin::new(88888, "uatom"), Coin::new(23456, "uxyz"), Coin::new(69420, "uusd"),]
    );
}

#[test]
fn receiving_funds() {
    let err = parse_received_fund(&[], "uxyz").unwrap_err();
    assert_eq!(err, StdError::generic_err("must deposit exactly one coin; received 0"));

    let err = parse_received_fund(&[Coin::new(12345, "uatom"), Coin::new(23456, "uxyz")], "uxyz")
        .unwrap_err();
    assert_eq!(err, StdError::generic_err("must deposit exactly one coin; received 2"));

    let err = parse_received_fund(&[Coin::new(12345, "uatom")], "uxyz").unwrap_err();
    assert_eq!(err, StdError::generic_err("expected uxyz deposit, received uatom"));

    let err = parse_received_fund(&[Coin::new(0, "uxyz")], "uxyz").unwrap_err();
    assert_eq!(err, StdError::generic_err("deposit amount must be non-zero"));

    let amount = parse_received_fund(&[Coin::new(69420, "uxyz")], "uxyz").unwrap();
    assert_eq!(amount, Uint128::new(69420));
}

#[test]
fn reconciling_underflow() {
    let mut deps = setup_test();
    let state = State::default();
    let previous_batch_list = vec![
        Batch {
            id: 1,
            reconciled: true,
            total_shares: Uint128::new(92876),
            amount_unclaimed: Uint128::new(95197), // 1.025 Token per Stake
            est_unbond_end_time: 10000,
        },
        Batch {
            id: 2,
            reconciled: false,
            total_shares: Uint128::new(1345),
            amount_unclaimed: Uint128::new(1385), // 1.030 Token per Stake
            est_unbond_end_time: 20000,
        },
        Batch {
            id: 3,
            reconciled: false,
            total_shares: Uint128::new(1456),
            amount_unclaimed: Uint128::new(1506), // 1.035 Token per Stake
            est_unbond_end_time: 30000,
        },
        Batch {
            id: 4,
            reconciled: false,
            total_shares: Uint128::new(1),
            amount_unclaimed: Uint128::new(1),
            est_unbond_end_time: 30001,
        },
    ];
    for previous_batch in &previous_batch_list {
        previous_batches().save(deps.as_mut().storage, previous_batch.id, previous_batch).unwrap();
    }
    state
        .unlocked_coins
        .save(
            deps.as_mut().storage,
            &vec![
                Coin::new(10000, "uatom"),
                Coin::new(234, "ukrw"),
                Coin::new(345, "uusd"),
                Coin::new(
                    69420,
                    "ibc/0471F1C4E7AFD3F07702BEF6DC365268D64570F7C1FDC98EA6098DD6DE59817B",
                ),
            ],
        )
        .unwrap();
    deps.querier.set_bank_balances(&[
        Coin::new(12345, "uatom"),
        Coin::new(234, "ukrw"),
        Coin::new(345, "uusd"),
        Coin::new(69420, "ibc/0471F1C4E7AFD3F07702BEF6DC365268D64570F7C1FDC98EA6098DD6DE59817B"),
    ]);
    execute(
        deps.as_mut(),
        mock_env_at_timestamp(35000),
        mock_info("worker", &[]),
        ExecuteMsg::Reconcile {},
    )
    .unwrap();
}

#[test]
fn reconciling_underflow_second() {
    let mut deps = setup_test();
    let state = State::default();
    let previous_batch_list = vec![
        Batch {
            id: 1,
            reconciled: true,
            total_shares: Uint128::new(92876),
            amount_unclaimed: Uint128::new(95197), // 1.025 Token per Stake
            est_unbond_end_time: 10000,
        },
        Batch {
            id: 2,
            reconciled: false,
            total_shares: Uint128::new(1345),
            amount_unclaimed: Uint128::new(1385), // 1.030 Token per Stake
            est_unbond_end_time: 20000,
        },
        Batch {
            id: 3,
            reconciled: false,
            total_shares: Uint128::new(176),
            amount_unclaimed: Uint128::new(183), // 1.035 Token per Stake
            est_unbond_end_time: 30000,
        },
        Batch {
            id: 4,
            reconciled: false,
            total_shares: Uint128::new(1),
            amount_unclaimed: Uint128::new(1),
            est_unbond_end_time: 30001,
        },
    ];
    for previous_batch in &previous_batch_list {
        previous_batches().save(deps.as_mut().storage, previous_batch.id, previous_batch).unwrap();
    }
    state
        .unlocked_coins
        .save(
            deps.as_mut().storage,
            &vec![
                Coin::new(10000, "uatom"),
                Coin::new(234, "ukrw"),
                Coin::new(345, "uusd"),
                Coin::new(
                    69420,
                    "ibc/0471F1C4E7AFD3F07702BEF6DC365268D64570F7C1FDC98EA6098DD6DE59817B",
                ),
            ],
        )
        .unwrap();
    deps.querier.set_bank_balances(&[
        Coin::new(12345 - 1323, "uatom"),
        Coin::new(234, "ukrw"),
        Coin::new(345, "uusd"),
        Coin::new(69420, "ibc/0471F1C4E7AFD3F07702BEF6DC365268D64570F7C1FDC98EA6098DD6DE59817B"),
    ]);
    execute(
        deps.as_mut(),
        mock_env_at_timestamp(35000),
        mock_info("worker", &[]),
        ExecuteMsg::Reconcile {},
    )
    .unwrap();
}
#[test]
fn dust_return_denom() {
    let mut deps = setup_test();

    // Bond when no delegation has been made
    // In this case, the full deposit simply goes to the first validator
    let res = execute(
        deps.as_mut(),
        mock_env(),
        mock_info("user_1", &[Coin::new(1_000_000, "uxyz")]),
        ExecuteMsg::Bond {
            receiver: None,
            exec_msg: None,
        },
    )
    .unwrap();

    // 3 messages. (switched to 3 so we can 'send' instead of 'transfer' minted tokens, so contract
    // will know about it 1 - delegate
    // 2 - mint token (to ourselves)
    // 3 - send/transfer it
    assert_eq!(res.messages.len(), 3);
    assert_eq!(
        res.messages[0],
        SubMsg {
            msg: Delegation::new("alice", 1_000_000, "uxyz").to_cosmos_msg(),
            id: REPLY_REGISTER_RECEIVED_COINS,
            gas_limit: None,
            reply_on: ReplyOn::Never
        }
    );
    let mint_msg = <MsgMint as Into<CosmosMsg>>::into(MsgMint {
        sender: "cosmos2contract".to_string(),
        amount: Some(denom::Coin {
            denom: "factory/cosmos2contract/boneXYZ".to_string(),
            amount: Uint128::new(1_000_000).to_string(),
        }),
    });
    assert_eq!(
        res.messages[1],
        SubMsg {
            id: 0,
            msg: mint_msg,
            gas_limit: None,
            reply_on: ReplyOn::Never,
        }
    );

    assert_eq!(
        res.messages[2],
        SubMsg {
            id: 0,
            msg: CosmosMsg::Bank(BankMsg::Send {
                to_address: "user_1".to_string(),
                amount: vec![Coin {
                    denom: "factory/cosmos2contract/boneXYZ".to_string(),
                    amount: Uint128::new(1_000_000)
                }],
            }),
            gas_limit: None,
            reply_on: ReplyOn::Never,
        }
    );

    // Bond when there are existing delegations, and Luna:Steak exchange rate is >1
    // Previously user 1 delegated 1,000,000 uluna. We assume we have accumulated 2.5% yield at
    // 1025000 staked
    deps.querier.set_staking_delegations(&[
        Delegation::new("alice", 341667, "uxyz"),
        Delegation::new("bob", 341667, "uxyz"),
        Delegation::new("charlie", 341666, "uxyz"),
    ]);

    // Charlie has the smallest amount of delegation, so the full deposit goes to him
    let res = execute(
        deps.as_mut(),
        mock_env(),
        mock_info("user_2", &[Coin::new(12345, "uxyz")]),
        ExecuteMsg::ReturnDenom {},
    )
    .unwrap();
    eprintln!("{:?}", res.messages);
    assert_eq!(res.messages.len(), 1);
    assert_eq!(
        res.messages[0],
        SubMsg {
            msg: Delegation::new("charlie", 12345, "uxyz").to_cosmos_msg(),
            id: REPLY_REGISTER_RECEIVED_COINS,
            gas_limit: None,
            reply_on: ReplyOn::Never
        }
    );
}
