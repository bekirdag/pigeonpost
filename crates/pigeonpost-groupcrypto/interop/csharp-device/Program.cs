using System.Text.Json;
using System.Text.Json.Nodes;
using Pigeonpost.GroupCrypto;

static string B64(byte[] b) => Convert.ToBase64String(b);
static byte[] Bytes(JsonNode? n) => Convert.FromBase64String(n!.GetValue<string>());

static JsonObject CommitJson(CommitBundle b) => new()
{
    ["epoch"] = b.Epoch, ["commit_id"] = b.CommitId, ["commit"] = B64(b.Commit),
    ["welcome"] = b.Welcome is null ? null : B64(b.Welcome), ["signature"] = B64(b.EnvelopeSignature),
};

static JsonObject AttJson(AttachmentRef a) => new() { ["key"] = B64(a.Key), ["sha"] = B64(a.SealedSha256), ["len"] = a.PlaintextLen };

static JsonObject IncomingJson(Incoming i) => i switch
{
    Incoming.Message m => new JsonObject
    {
        ["type"] = "message", ["body"] = B64(m.MessageValue.Body), ["sender_mailbox"] = m.MessageValue.Sender.Mailbox,
        ["sender_device"] = m.MessageValue.Sender.DeviceId, ["client_message_id"] = m.MessageValue.ClientMessageId,
        ["epoch"] = m.MessageValue.Epoch,
        ["attachments"] = new JsonArray(m.MessageValue.Attachments.Select(a => (JsonNode)AttJson(a)).ToArray()),
    },
    Incoming.Commit c => new JsonObject
    {
        ["type"] = "commit", ["epoch"] = c.Outcome.Epoch, ["self_removed"] = c.Outcome.SelfRemoved,
        ["committer_mailbox"] = c.Outcome.Committer.Mailbox, ["added"] = c.Outcome.Added.Length,
        ["removed"] = c.Outcome.Removed.Length,
    },
    Incoming.OwnCommitConfirmed o => new JsonObject { ["type"] = "own_commit_confirmed", ["epoch"] = o.Outcome.Epoch },
    Incoming.HistoryOffer => new JsonObject { ["type"] = "history_offer" },
    _ => new JsonObject { ["type"] = "own" },
};

GroupDevice? device = null;

JsonObject Handle(JsonObject r)
{
    var g = r["group"]?.GetValue<string>() ?? "";
    var d = device;
    switch (r["op"]!.GetValue<string>())
    {
        case "init":
            device = GroupDevice.NewWithIdentitySeed(Bytes(r["seed"]), r["device"]!.GetValue<string>());
            return new JsonObject { ["mailbox"] = device.Mailbox(), ["signature_key"] = B64(device.SignaturePublicKey()) };
        case "key_packages":
            var kps = d!.GenerateKeyPackages(r["count"]!.GetValue<uint>()).ToList();
            kps.Add(d.GenerateLastResortKeyPackage());
            return new JsonObject { ["key_packages"] = new JsonArray(kps.Select(k => (JsonNode)B64(k)).ToArray()) };
        case "create":
            return new JsonObject { ["epoch"] = d!.CreateGroup(g).Epoch };
        case "add":
            return CommitJson(d!.AddMembers(g, r["key_packages"]!.AsArray().Select(Bytes).ToArray()));
        case "remove_mailbox":
            return CommitJson(d!.RemoveMailbox(g, r["mailbox"]!.GetValue<string>()));
        case "self_update":
            return CommitJson(d!.SelfUpdate(g));
        case "confirm":
            return new JsonObject { ["epoch"] = d!.ConfirmCommit(g).Epoch };
        case "discard":
            d!.DiscardCommit(g);
            return new JsonObject();
        case "join":
            var j = d!.JoinFromWelcome(Bytes(r["welcome"]));
            return new JsonObject { ["group"] = j.GroupId, ["epoch"] = j.Epoch, ["members"] = j.Members.Length };
        case "encrypt":
        {
            var atts = new List<AttachmentRef>();
            string? sealedB64 = null;
            if (r["attachment"] is JsonValue)
            {
                var s = GroupCrypto.SealAttachment(Bytes(r["attachment"]));
                sealedB64 = B64(s.Sealed);
                atts.Add(s.Reference);
            }
            var m = d!.EncryptMessage(g, r["id"]!.GetValue<string>(), Bytes(r["body"]), atts.ToArray());
            return new JsonObject
            {
                ["ciphertext"] = B64(m.Ciphertext), ["signature"] = B64(m.EnvelopeSignature), ["epoch"] = m.Epoch, ["sealed"] = sealedB64,
            };
        }
        case "process":
            return IncomingJson(d!.ProcessIncoming(g, Bytes(r["message"])));
        case "open_attachment":
        {
            var refj = r["ref"]!;
            var a = new AttachmentRef(Bytes(refj["key"]), Bytes(refj["sha"]), refj["len"]!.GetValue<ulong>());
            return new JsonObject { ["plaintext"] = B64(GroupCrypto.OpenAttachment(Bytes(r["sealed"]), a)) };
        }
        case "status":
            return new JsonObject
            {
                ["epoch"] = d!.Epoch(g), ["epoch_authenticator"] = B64(d.EpochAuthenticator(g)),
                ["safety_number"] = d.SafetyNumber(g), ["members"] = d.Members(g).Length,
            };
        case "persist":
        {
            var key = GroupCrypto.GenerateStateKey();
            var blob = d!.ExportState(key);
            device = GroupDevice.ImportState(blob, key);
            d.Dispose();
            return new JsonObject { ["bytes"] = blob.Length };
        }
        case "snapshot":
        {
            var key = GroupCrypto.GenerateStateKey();
            return new JsonObject { ["state"] = B64(d!.ExportState(key)), ["key"] = B64(key) };
        }
        case "probe_snapshot":
        {
            using var p = GroupDevice.ImportState(Bytes(r["state"]), Bytes(r["key"]));
            try
            {
                return new JsonObject { ["code"] = "ok", ["result"] = IncomingJson(p.ProcessIncoming(g, Bytes(r["message"]))) };
            }
            catch (GroupCryptoException.Failure e)
            {
                return new JsonObject { ["code"] = e.code };
            }
        }
        default:
            throw new GroupCryptoException.Failure("invalid_input", "unknown op");
    }
}

string? line;
var stdout = Console.Out;
while ((line = Console.ReadLine()) != null)
{
    var req = JsonNode.Parse(line)!.AsObject();
    if (req["op"]!.GetValue<string>() == "quit") break;
    JsonObject resp;
    try
    {
        resp = Handle(req);
        resp["ok"] = true;
    }
    catch (GroupCryptoException.Failure e)
    {
        resp = new JsonObject { ["ok"] = false, ["code"] = e.code, ["message"] = e.detail };
    }
    stdout.WriteLine(resp.ToJsonString());
    stdout.Flush();
}
