"""Exercises the `game` bindings: `GameId` parsing and its per-game facts."""

import blam_tags as bt


def test_game_ids_read_back_as_themselves():
    ids = ["haloce_mcc", "halo2_mcc", "halo2amp_mcc", "halo3_mcc", "halo3odst_mcc",
           "haloreach_mcc", "halo4_mcc", "haloce_evolved"]
    for id in ids:
        assert bt.GameId.from_id(id).as_str() == id
    assert bt.GameId.from_id("halo5_mcc") is None


def test_game_facts():
    assert bt.GameId.from_id("haloce_mcc") == bt.GameId.HaloCe
    assert bt.GameId.HaloCe.is_classic()
    assert not bt.GameId.HaloReach.is_classic()
    assert bt.GameId.CampaignEvolved.is_campaign_evolved()
    assert bt.GameId.CampaignEvolved.generation() == bt.Game.Halo3
    assert bt.GameId.Halo2.generation() == bt.Game.Halo2
