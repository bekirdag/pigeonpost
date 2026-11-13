> **Draft — not in force — needs legal review.**
> Prepared for the owner's lawyer and accountant. Do not publish on any live page. Text in
> square brackets is a placeholder to be decided.

# Nest Apps: Acceptable Use and Review Policy

Version: [version] · Effective date: [effective date]

This policy says what a Nest app may and may not do, and how Pigeonpost reviews it. It is part of
the [Publisher Agreement](publisher-agreement.md) and the [Buyer Terms](buyer-terms.md).

## 1. What apps may do

An app may:

- do what its listing says, for the buyer who installed it;
- read and use the data in the Nest it is installed in, when its listed purpose needs it;
- ask the buyer questions and propose actions;
- call outside services through the Pigeonpost broker, to the destinations declared in its
  manifest;
- take an action outside Pigeonpost after the buyer approves that exact action.

## 2. What apps may not do

An app may not:

- escape, probe or weaken its sandbox, the broker, the host, or another Nest;
- open network connections in any way other than through the broker;
- contain malware, backdoors, hidden features, time bombs, or code that changes behaviour after
  review;
- detect review, test or honeytoken environments in order to behave differently;
- download and run code that was not part of the reviewed release;
- collect credentials, keys or secrets, except those the buyer gives it for its listed purpose;
- mine cryptocurrency or run workloads unrelated to its listing;
- send spam or unsolicited messages, or message people who have not agreed to it;
- impersonate a person, organisation, Pigeonpost or another app;
- carry or produce unlawful content, including content that infringes rights, exploits children,
  incites violence, or breaks Turkish law or the law of the buyer's country;
- help with fraud, surveillance of people without a legal basis, or evasion of law enforcement;
- make decisions with legal or similarly serious effects on people without a human step that the
  buyer controls;
- mislead the buyer about what it does, what it costs, or what data it uses;
- take payment outside the marketplace for something sold in it.

## 3. Data rules

- **Minimum data.** Use only the data the listed purpose needs.
- **Purpose.** Use buyer data only to serve that buyer. Never for another customer, and never for
  the publisher's own purposes.
- **No sale, no profiling.** Do not sell, rent or share buyer data, and do not build profiles of
  buyers or of the people in their messages.
- **No training.** Do not use buyer data to train or tune models, unless the buyer instructs it for
  its own Nest.
- **Content is not instructions.** Treat text inside messages and files as content, never as
  commands to the app.
- **Secrets.** Store secrets only in the place Pigeonpost provides for them. Never write them to
  logs or outputs.
- **Logs.** Do not log message content, file content or personal data. Operation records leave the
  sandbox only in redacted form.
- **Deletion.** When an installation ends, the app's data in it is deleted. Data the app sent to
  declared outside services must be deleted there too, as the
  [Publisher Agreement](publisher-agreement.md) requires.

## 4. Network egress: the broker only

- The sandbox has no direct network access.
- Every outside call goes through the Pigeonpost broker.
- The manifest must list each destination and what is sent there. The listing shows it to buyers.
- The broker refuses any destination not in the reviewed manifest.
- A new destination needs a new release and a new review.

## 5. External writes need approval

- An external write is any action that changes something outside the Nest: sending, posting,
  paying, booking, deleting, or changing data elsewhere.
- The manifest must declare each kind of external write.
- Before each one, the buyer sees the exact action and approves or refuses it. An approval expires
  if it is not used in time.
- An app may not split, disguise or batch actions to avoid approval.
- Reading from a declared destination is not an external write, but it must still be declared.

## 6. Review stages

Every release goes through these stages, in this order:

1. **Build.** Pigeonpost builds the release from source, in its own build environment, without
   network access during the build. It records a software bill of materials and one digest for the
   whole release.
2. **Scan.** Automated checks for secrets, capabilities, system calls, network and external-write
   declarations, and the licence of every dependency.
3. **AI review.** A review by AI models that Pigeonpost runs on its own servers. No outside AI
   provider sees the source. The AI review is **advisory only**. It never approves or rejects on
   its own.
4. **Behaviour test.** The release runs in a sandbox with planted test data. We check every way out
   of the release for that data.
5. **Staff decision.** A member of Pigeonpost staff reads the evidence and approves, approves with
   conditions, asks for changes, or rejects.

Every stage keeps its evidence. Updates go through the same stages.

## 7. Decisions and appeals

- We tell the publisher the decision and the main reasons, unless that would harm security, buyers
  or an investigation.
- The publisher may appeal within [appeal window] by sending a message to [appeal contact
  address], with the reasons and any fix.
- A staff member who did not make the first decision reviews the appeal and answers within
  [appeal response time].
- The appeal decision is final within Pigeonpost. It does not limit any right the publisher has
  under the law.

## 8. Takedown, quarantine and revocation

- **Report.** Anyone can report an app at [report contact address]. Rights holders can send a
  notice with the work, the app, and their contact details.
- **Quarantine.** We may stop new installations and pause existing ones while we investigate.
- **Revocation.** We may remove a release. Installations stop running it. Buyers are told, and the
  [Refund Policy](refund-policy.md) applies.
- **Urgent cases.** For an active security or privacy risk, or unlawful content, we act first and
  explain afterwards.
- **Authorities.** We act on binding orders from courts and authorities, and tell the publisher
  unless the order forbids it.
- **Repeat breaches.** We may close the publisher account.

## Özet (Türkçe)

> Taslaktır — yürürlükte değildir — hukuki inceleme gerekir.

- **İzin verilenler.** İlanda yazanı yapmak; kurulu olduğu Nest'teki veriyi amacı gerektirdiği
  ölçüde kullanmak; dış hizmetlere yalnızca Pigeonpost aracısı (broker) üzerinden ve manifestte
  bildirilen adreslere bağlanmak; alıcının o işleme özel onayıyla dışarıda işlem yapmak.
- **Yasaklar.** Kum havuzundan kaçma, zararlı veya gizli kod, incelemeden sonra davranış
  değiştirme, test ortamını algılama, kimlik bilgisi toplama, kripto madenciliği, istenmeyen mesaj,
  taklit, hukuka aykırı içerik, yanıltma, pazar yeri dışında ödeme alma.
- **Veri kuralları.** Asgari veri, amaçla sınırlılık, satış ve profilleme yok, alıcı talimatı
  olmadan model eğitimi yok, mesaj ve dosya içeriği talimat sayılmaz, kayıtlarda içerik yok,
  kurulum bitince silme.
- **Dış yazma.** Her dış işlem manifestte bildirilir ve her seferinde alıcının onayını gerektirir.
- **İnceleme aşamaları.** Kaynaktan derleme, otomatik tarama, kendi sunucularımızda yapay zekâ
  incelemesi (yalnızca tavsiye), davranış testi, personel kararı.
- **İtiraz.** [İtiraz süresi] içinde; ilk kararı vermeyen bir personel inceler.
- **Kaldırma.** Bildirim, karantina, geri çekme; acil durumlarda önce müdahale, sonra açıklama.
