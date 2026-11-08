#!/usr/bin/env python3
"""Offline contract tests; no API, checkout, host or provider is launched."""
import ast
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

HERE=Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location("collector",HERE/"collect.py")
collector=importlib.util.module_from_spec(spec)
spec.loader.exec_module(collector)
audit_spec=importlib.util.spec_from_file_location("source_audit",HERE/"audit-source-set.py")
auditor=importlib.util.module_from_spec(audit_spec)
audit_spec.loader.exec_module(auditor)

class DependencyMutations(unittest.TestCase):
    def dependency(self):
        return {"git":"https://github.com/beamiter/jterm_core.git","rev":collector.CORE}
    def manifest(self):
        return {"dependencies":{"jterm_core":self.dependency()}}
    def rejected(self,manifest):
        with self.assertRaises(ValueError):auditor.manifest_pins(manifest,collector.CORE)
    def test_explicit_matching_root_dependencies_are_valid(self):
        manifest=self.manifest();manifest["dev-dependencies"]={"jterm_core":self.dependency()}
        self.assertEqual(len(auditor.manifest_pins(manifest,collector.CORE)),2)
    def test_workspace_or_test_only_pin_cannot_replace_production_dependency(self):
        for manifest in ({"workspace":{"dependencies":{"jterm_core":self.dependency()}}},{"dev-dependencies":{"jterm_core":self.dependency()}},{"target":{"cfg(unix)":{"dependencies":{"jterm_core":self.dependency()}}}}):
            self.rejected(manifest)
    def test_root_alias_is_rejected(self):
        manifest=self.manifest();manifest["dependencies"]["hidden_core"]={**self.dependency(),"package":"jterm_core"};self.rejected(manifest)
    def test_target_alias_is_rejected(self):
        manifest=self.manifest();manifest["target"]={"cfg(unix)":{"dependencies":{"hidden_core":{**self.dependency(),"package":"jterm_core"}}}};self.rejected(manifest)
    def test_workspace_alias_is_rejected(self):
        manifest=self.manifest();manifest["workspace"]={"dependencies":{"hidden_core":{**self.dependency(),"package":"jterm_core"}}};self.rejected(manifest)
    def test_target_build_and_dev_aliases_are_rejected(self):
        for kind in ("build-dependencies","dev-dependencies"):
            manifest=self.manifest();manifest["target"]={"x86_64-unknown-linux-gnu":{kind:{"hidden_core":{**self.dependency(),"package":"jterm_core"}}}};self.rejected(manifest)
    def test_target_same_name_different_pin_is_rejected(self):
        manifest=self.manifest();manifest["target"]={"cfg(windows)":{"dependencies":{"jterm_core":{**self.dependency(),"rev":"0"*40}}}};self.rejected(manifest)
    def test_workspace_same_name_different_pin_is_rejected(self):
        manifest=self.manifest();manifest["workspace"]={"dependencies":{"jterm_core":{**self.dependency(),"rev":"0"*40}}};self.rejected(manifest)
    def test_matching_explicit_workspace_and_target_pins_are_valid(self):
        manifest=self.manifest();manifest["workspace"]={"dependencies":{"jterm_core":self.dependency()}};manifest["target"]={"cfg(unix)":{"build-dependencies":{"jterm_core":self.dependency()}}}
        self.assertEqual(len(auditor.manifest_pins(manifest,collector.CORE)),3)
    def test_workspace_inheritance_is_not_mistaken_for_explicit_pin(self):
        manifest={"dependencies":{"jterm_core":{"workspace":True}},"workspace":{"dependencies":{"jterm_core":self.dependency()}}};self.rejected(manifest)
    def test_same_name_different_package_is_rejected(self):
        manifest=self.manifest();manifest["dependencies"]["jterm_core"]["package"]="another_package";self.rejected(manifest)
    def test_core_git_alias_without_package_is_rejected(self):
        manifest=self.manifest();manifest["workspace"]={"dependencies":{"hidden_core":self.dependency()}};self.rejected(manifest)
    def test_documentation_metadata_is_not_a_cargo_dependency(self):
        manifest=self.manifest();manifest["package"]={"metadata":{"example":{"dependencies":{"jterm_core":{"path":"not-a-build-input"}}}}}
        self.assertEqual(len(auditor.manifest_pins(manifest,collector.CORE)),1)
    def test_patch_alias_is_rejected(self):
        manifest={"patch":{"https://github.com/beamiter/jterm_core.git":{"hidden_core":{"package":"jterm_core","path":"../core"}}}}
        with self.assertRaisesRegex(ValueError,"core patch"):auditor.reject_overrides(manifest,"Cargo.toml")

class Contract(unittest.TestCase):
    def setUp(self):
        self.manifest=json.loads((HERE/"frozen-candidates.json").read_text())
        self.manifest["frozen"]=True
        self.digest=collector.digest_set(self.manifest)
        self.item=self.manifest["repositories"][0]
    def runs(self):
        return {"total_count":len(collector.WORKFLOWS[self.item["name"]]),"workflow_runs":[{"id":i+1,"run_attempt":1,"path":".github/workflows/"+path,"head_sha":self.item["revision"],"event":"push","head_branch":"master","status":"completed","conclusion":"success"} for i,path in enumerate(collector.WORKFLOWS[self.item["name"]])]}
    def reject(self,manifest):
        with self.assertRaises(ValueError):collector.validate_manifest(manifest,self.digest)
    def test_exact_authorized_set_is_valid(self):
        self.assertEqual(collector.validate_manifest(self.manifest,self.digest),self.digest)
    def test_preparation_cannot_run(self):
        self.manifest["frozen"]=False;self.reject(self.manifest)
    def test_truthy_string_is_not_frozen(self):
        self.manifest["frozen"]="true";self.reject(self.manifest)
    def test_extra_input_data_is_rejected(self):
        self.manifest["arbitrary_notes"]="Not allowed in a public source contract";self.reject(self.manifest)
    def test_extra_repository_data_is_rejected(self):
        self.manifest["repositories"][0]["url"]="https://example.invalid";self.reject(self.manifest)
    def test_other_repository_is_rejected(self):
        self.manifest["repositories"][1]["full_name"]="other/anvil";self.reject(self.manifest)
    def test_duplicate_identity_is_rejected(self):
        self.manifest["repositories"][1]=copy.deepcopy(self.manifest["repositories"][0]);self.reject(self.manifest)
    def test_missing_repository_is_rejected(self):
        self.manifest["repositories"].pop();self.reject(self.manifest)
    def test_abbreviated_sha_is_rejected(self):
        self.manifest["repositories"][1]["revision"]="abcdef1";self.reject(self.manifest)
    def test_shell_text_is_rejected(self):
        self.manifest["repositories"][1]["revision"]="$(echo unsafe)";self.reject(self.manifest)
    def test_changed_core_requires_deliberate_harness_update(self):
        self.manifest["repositories"][0]["revision"]="0"*40;self.reject(self.manifest)
    def test_changed_set_digest_is_rejected(self):
        self.manifest["repositories"][1]["revision"]="0"*40;self.reject(self.manifest)
    def test_order_does_not_change_candidate_identity(self):
        self.manifest["repositories"].reverse()
        self.assertEqual(collector.validate_manifest(self.manifest,self.digest),self.digest)
    def test_exact_successful_runs_are_selected(self):
        self.assertEqual(len(collector.select_runs(self.item,self.runs())),3)
    def test_missing_workflow_does_not_pass(self):
        data=self.runs();data["workflow_runs"].pop()
        with self.assertRaises(ValueError):collector.select_runs(self.item,data)
    def test_old_head_success_does_not_pass(self):
        data=self.runs();data["workflow_runs"][0]["head_sha"]="0"*40
        with self.assertRaises(ValueError):collector.select_runs(self.item,data)
    def test_pr_success_does_not_replace_push(self):
        data=self.runs();data["workflow_runs"][0]["event"]="pull_request"
        with self.assertRaises(ValueError):collector.select_runs(self.item,data)
    def test_manual_run_cannot_replace_ordinary_push_gate(self):
        data=self.runs();data["workflow_runs"][0]["event"]="workflow_dispatch"
        with self.assertRaises(ValueError):collector.select_runs(self.item,data)
    def test_other_branch_same_commit_does_not_pass(self):
        data=self.runs();data["workflow_runs"][0]["head_branch"]="candidate"
        with self.assertRaises(ValueError):collector.select_runs(self.item,data)
    def anvil_runs(self):
        self.item=next(r for r in self.manifest["repositories"] if r["name"]=="anvil")
        data=self.runs()
        link=next(r for r in data["workflow_runs"] if r["path"].endswith("/link-recovery.yml"))
        link["event"]="workflow_dispatch"
        return data,link
    def test_only_anvil_link_recovery_allows_manual_exact_master(self):
        data,link=self.anvil_runs()
        selected=collector.select_runs(self.item,data)
        self.assertEqual(len(selected),5)
        self.assertTrue(any(all(r[k]==v for k,v in link.items()) for r in selected))
        self.assertEqual(collector.MANUAL_WORKFLOWS,{("jterm_core","security.yml"),("anvil","link-recovery.yml"),("forge","flatpak.yml")})
    def test_forge_flatpak_allows_only_successful_exact_master_manual_run(self):
        self.item=next(r for r in self.manifest["repositories"] if r["name"]=="forge")
        data=self.runs();flatpak=next(r for r in data["workflow_runs"] if r["path"].endswith("/flatpak.yml"))
        flatpak["event"]="workflow_dispatch"
        self.assertEqual(len(collector.select_runs(self.item,data)),4)
        for key,value in (("head_sha","0"*40),("head_branch","candidate"),("event","pull_request"),("status","in_progress"),("conclusion","failure"),("conclusion","skipped")):
            changed=copy.deepcopy(data)
            next(r for r in changed["workflow_runs"] if r["path"].endswith("/flatpak.yml"))[key]=value
            with self.assertRaises(ValueError):collector.select_runs(self.item,changed)
    def test_forge_other_workflows_cannot_substitute_manual_run(self):
        self.item=next(r for r in self.manifest["repositories"] if r["name"]=="forge")
        data=self.runs();data["workflow_runs"][0]["event"]="workflow_dispatch"
        with self.assertRaises(ValueError):collector.select_runs(self.item,data)
    def test_newer_flatpak_failure_cannot_hide_behind_older_green(self):
        self.item=next(r for r in self.manifest["repositories"] if r["name"]=="forge")
        for newer_event,older_event in (("workflow_dispatch","push"),("push","workflow_dispatch")):
            data=self.runs();old=next(r for r in data["workflow_runs"] if r["path"].endswith("/flatpak.yml"));old["event"]=older_event
            new=copy.deepcopy(old);new.update(id=100,event=newer_event,conclusion="failure");data["workflow_runs"].append(new)
            with self.assertRaises(ValueError):collector.select_runs(self.item,data)
    def test_manual_link_recovery_rejects_wrong_head_and_branch(self):
        for key,value in (("head_sha","0"*40),("head_branch","candidate"),("event","pull_request")):
            data,link=self.anvil_runs();link[key]=value
            with self.assertRaises(ValueError):collector.select_runs(self.item,data)
    def test_manual_link_recovery_never_accepts_incomplete_or_failed(self):
        for status,conclusion in (("queued",None),("in_progress",None),("completed","failure"),("completed","cancelled"),("completed","skipped")):
            data,link=self.anvil_runs();link.update(status=status,conclusion=conclusion)
            with self.assertRaises(ValueError):collector.select_runs(self.item,data)
    def test_newer_manual_link_failure_cannot_hide_behind_green_push(self):
        data,link=self.anvil_runs();old=copy.deepcopy(link);old.update(id=0,event="push")
        data["workflow_runs"].append(old);link["conclusion"]="failure"
        with self.assertRaises(ValueError):collector.select_runs(self.item,data)
    def test_newer_push_link_failure_cannot_hide_behind_green_manual(self):
        data,link=self.anvil_runs();new=copy.deepcopy(link);new.update(id=100,event="push",conclusion="failure")
        data["workflow_runs"].append(new)
        with self.assertRaises(ValueError):collector.select_runs(self.item,data)
    def test_pending_failure_cancelled_and_skipped_are_not_pass(self):
        for status,conclusion in [("in_progress",None),("queued",None),("completed","failure"),("completed","cancelled"),("completed","skipped")]:
            data=self.runs();data["workflow_runs"][0].update(status=status,conclusion=conclusion)
            with self.assertRaises(ValueError):collector.select_runs(self.item,data)
    def test_newer_failure_cannot_hide_behind_older_green(self):
        data=self.runs();new=copy.deepcopy(data["workflow_runs"][0]);new.update(id=100,conclusion="failure");data["workflow_runs"].append(new)
        with self.assertRaises(ValueError):collector.select_runs(self.item,data)
    def test_unbounded_pagination_is_not_assumed_complete(self):
        data=self.runs();data["total_count"]=101
        with self.assertRaises(ValueError):collector.select_runs(self.item,data)
    def test_unfrozen_stops_before_network_and_files(self):
        with tempfile.TemporaryDirectory() as temp:
            manifest=Path(temp)/"input.json";self.manifest["frozen"]=False;manifest.write_text(json.dumps(self.manifest))
            output=Path(temp)/"new-run"
            with mock.patch.object(collector,"receipts") as remote:
                with self.assertRaises(ValueError):collector.prepare(output,manifest,self.digest)
                remote.assert_not_called()
            self.assertFalse(output.exists())
    def test_changed_master_ref_stops_receipts(self):
        response={"ref":"refs/heads/master","object":{"type":"commit","sha":"0"*40}}
        with mock.patch.object(collector,"fetch",return_value=response):
            with self.assertRaisesRegex(ValueError,"master moved"):collector.receipts(self.manifest)
    def test_dispatch_from_unreviewed_ref_stops_before_network(self):
        with tempfile.TemporaryDirectory() as temp:
            path=Path(temp)/"input.json";path.write_text(json.dumps(self.manifest))
            with mock.patch.dict(collector.os.environ,{"GITHUB_ACTIONS":"true","GITHUB_REF":"refs/heads/other","GITHUB_REPOSITORY":"beamiter/jterm_core","GITHUB_SHA":collector.CORE}), mock.patch.object(collector,"receipts") as remote:
                with self.assertRaisesRegex(ValueError,"reviewed master"):collector.prepare(Path(temp)/"run",path,self.digest)
                remote.assert_not_called()
    def test_dispatch_from_different_commit_stops_before_network(self):
        with tempfile.TemporaryDirectory() as temp:
            path=Path(temp)/"input.json";path.write_text(json.dumps(self.manifest))
            with mock.patch.dict(collector.os.environ,{"GITHUB_ACTIONS":"true","GITHUB_REF":"refs/heads/master","GITHUB_REPOSITORY":"beamiter/jterm_core","GITHUB_SHA":"0"*40}), mock.patch.object(collector,"receipts") as remote:
                with self.assertRaisesRegex(ValueError,"reviewed master"):collector.prepare(Path(temp)/"run",path,self.digest)
                remote.assert_not_called()
    def test_oversized_input_stops_before_network(self):
        with tempfile.TemporaryDirectory() as temp:
            path=Path(temp)/"input.json";path.write_text(" "*65537)
            with mock.patch.object(collector,"receipts") as remote:
                with self.assertRaisesRegex(ValueError,"budget"):collector.prepare(Path(temp)/"run",path,self.digest)
                remote.assert_not_called()
    def test_acceptance_scripts_do_not_depend_on_optimizable_asserts(self):
        for file in ("collect.py","audit-source-set.py"):
            self.assertFalse(any(isinstance(node,ast.Assert) for node in ast.walk(ast.parse((HERE/file).read_text()))))
    def test_workflow_is_manual_read_only_and_has_no_secret_inputs(self):
        workflow=(HERE.parents[1]/".github/workflows/five-repository-host.yml").read_text()
        self.assertIn("workflow_dispatch:",workflow)
        self.assertIn("contents: read",workflow)
        self.assertIn("persist-credentials: false",workflow)
        for forbidden in ("pull_request_target:","push:","secrets.","continue-on-error:","contents: write","id-token: write","sudo "):
            self.assertNotIn(forbidden,workflow)
    def test_all_rust_commands_explicitly_select_the_recorded_toolchain(self):
        workflow=(HERE.parents[1]/".github/workflows/five-repository-host.yml").read_text()
        self.assertIn('RUSTUP_TOOLCHAIN: "1.86.0"',workflow)
        self.assertNotIn('rustup default',workflow)
        self.assertNotRegex(workflow,r'(?m)^\s*(?:cargo|rustc)\s+(?!\+1\.86\.0)')
        for operation in ('fmt','clippy','test','build'):
            self.assertIn(f'cargo +1.86.0 {operation}',workflow)
        self.assertIn('rustc +1.86.0 -vV',workflow)
        self.assertIn('rustup run 1.86.0 rustfmt --version',workflow)
        self.assertIn('rustup run 1.86.0 clippy-driver --version',workflow)


class CoreSecurityGateTests(unittest.TestCase):
    def setUp(self):
        self.manifest=json.loads((HERE/"frozen-candidates.json").read_text())
        self.manifest["frozen"]=True
        self.item=next(r for r in self.manifest["repositories"] if r["name"]=="jterm_core")
        self.digest=collector.digest_set(self.manifest)
    def runs(self):
        return {"total_count":3,"workflow_runs":[{"id":i+1,"run_attempt":1,"path":".github/workflows/"+path,"head_sha":self.item["revision"],"event":"push","head_branch":"master","status":"completed","conclusion":"success"} for i,path in enumerate(collector.WORKFLOWS["jterm_core"])]}
    def security(self,data):
        return next(r for r in data["workflow_runs"] if r["path"]==".github/workflows/security.yml")
    def test_core_security_is_a_required_base_gate(self):
        self.assertEqual(sum(map(len,collector.WORKFLOWS.values())),20)
        self.assertEqual(collector.WORKFLOWS["jterm_core"],("ci.yml","vim9-pixel-input.yml","security.yml"))
        for event in ("push","workflow_dispatch"):
            data=self.runs();self.security(data)["event"]=event
            self.assertEqual(len(collector.select_runs(self.item,data)),3)
    def test_missing_wrong_identity_or_event_security_fails(self):
        data=self.runs();data["workflow_runs"].remove(self.security(data))
        with self.assertRaises(ValueError):collector.select_runs(self.item,data)
        for key,value in (("head_sha","0"*40),("head_branch","other"),("path",".github/workflows/other.yml"),("event","pull_request"),("event","schedule"),("event","workflow_run")):
            for event in ("push","workflow_dispatch"):
                data=self.runs();self.security(data).update(event=event);self.security(data)[key]=value
                with self.subTest(key=key,value=value,event=event),self.assertRaises(ValueError):collector.select_runs(self.item,data)
    def test_latest_security_failure_and_rerun_cannot_fall_back(self):
        for status,conclusion in (("queued",None),("in_progress",None),("completed","failure"),("completed","cancelled"),("completed","skipped")):
            for newer in ({"id":100},{"id":3,"run_attempt":2}):
                for old_event,new_event in (("push","workflow_dispatch"),("workflow_dispatch","push")):
                    data=self.runs();old=self.security(data);old["event"]=old_event
                    new=copy.deepcopy(old);new.update(**newer,event=new_event,status=status,conclusion=conclusion);data["workflow_runs"].append(new)
                    with self.subTest(status=status,newer=newer,events=(old_event,new_event)),self.assertRaises(ValueError):collector.select_runs(self.item,data)
    def fake_fetch_with_failed_security(self,full_name,suffix):
        self.assertEqual(full_name,"beamiter/jterm_core")
        if suffix=="git/ref/heads/master":
            return {"ref":"refs/heads/master","object":{"type":"commit","sha":self.item["revision"]}}
        self.assertEqual(suffix,f"actions/runs?head_sha={self.item['revision']}&per_page=100")
        data=self.runs();self.security(data)["conclusion"]="failure";return data
    def test_failed_core_security_stops_prepare_before_checkout(self):
        with tempfile.TemporaryDirectory() as temp:
            path=Path(temp)/"input.json";path.write_text(json.dumps(self.manifest));output=Path(temp)/"run"
            with mock.patch.dict(collector.os.environ,{"GITHUB_ACTIONS":"false"}),mock.patch.object(collector,"fetch",side_effect=self.fake_fetch_with_failed_security),mock.patch.object(collector.subprocess,"run") as execute:
                with self.assertRaisesRegex(ValueError,"security.yml"):collector.prepare(output,path,self.digest)
                execute.assert_not_called();self.assertFalse(output.exists())
    def test_failed_core_security_stops_finish_before_credit(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);(root/"frozen-candidates.json").write_text(json.dumps(self.manifest))
            with mock.patch.object(collector,"source_preflight"),mock.patch.object(collector,"fetch",side_effect=self.fake_fetch_with_failed_security):
                with self.assertRaisesRegex(ValueError,"security.yml"):collector.finish(root,self.digest)
            self.assertFalse((root/"evidence/final-receipt.json").exists())


class RecoveredIntegrationContract(unittest.TestCase):
    def test_all_compiled_core_sources_and_lock_agree(self):
        import tomllib
        manifest=tomllib.loads((HERE/"harness/Cargo.toml").read_text())
        self.assertEqual(manifest["dependencies"]["jterm_core"]["rev"],collector.CORE)
        lock=tomllib.loads((HERE/"harness/Cargo.lock").read_text())
        entries=[p for p in lock["package"] if p["name"]=="jterm_core"]
        self.assertEqual(len(entries),1)
        self.assertEqual(entries[0]["source"],f"git+https://github.com/beamiter/jterm_core.git?rev={collector.CORE}#{collector.CORE}")
        for name in ("main.rs","bin/mission-validation-continuation.rs","bin/mission-native-state.rs"):
            self.assertIn(f'const CORE: &str = "{collector.CORE}";', (HERE/"harness/src"/name).read_text())
    def test_native_reader_core_agrees_without_executing_it(self):
        tree=ast.parse((HERE/"native/reader.py").read_text())
        constants=[ast.literal_eval(node.value) for node in tree.body if isinstance(node,ast.Assign) and any(isinstance(t,ast.Name) and t.id=="CORE" for t in node.targets)]
        self.assertEqual(constants,[collector.CORE])
    def test_both_examples_are_unfrozen_and_runtime_identity_blank(self):
        candidate=json.loads((HERE/"frozen-candidates.json").read_text())
        validation=json.loads((HERE/"validation-input.example.json").read_text())
        self.assertIs(candidate["frozen"],False);self.assertIs(validation["frozen"],False)
        self.assertEqual(validation["core"],collector.CORE)
        for key in ("state_dir","mission_id"):
            self.assertEqual(validation[key],"")
        self.assertEqual(validation["mission_revision"],0)
    def test_real_task_cards_are_not_run_and_disallow_substitutes(self):
        cards=json.loads((HERE/"acceptance-cards.json").read_text())
        self.assertEqual(cards["status"],"not_run")
        self.assertIs(cards["candidate_set_frozen"],False)
        self.assertEqual({c["id"] for c in cards["cards"]},{"T02","T09","T10"})
        self.assertEqual(cards["repetitions_per_task"],3)
        for card in cards["cards"]:
            self.assertEqual(card["status"],"not_run")
            self.assertGreaterEqual(len(card["non_substitutes"]),3)
    def test_production_evidence_reducer_is_test_only(self):
        main=(HERE/"harness/src/main.rs").read_text()
        self.assertIn('#[cfg(test)]\n#[path = "../../../../src/ui/workspace_mission_evidence.rs"]\nmod forge_mission_evidence;',main)
        self.assertTrue((HERE.parents[1]/"src/ui/workspace_mission_evidence.rs").is_file())
        self.assertIn('evidence.poll(&[], false)',main)

if __name__=="__main__":unittest.main()
