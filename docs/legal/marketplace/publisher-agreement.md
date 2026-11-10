> **Draft — not in force — needs legal review.**
> Prepared for the owner's lawyer and accountant. Do not publish on any live page. Text in
> square brackets is a placeholder to be decided.

# Nests Marketplace Publisher Agreement

Version: [version] · Effective date: [effective date]

This agreement is between you, the publisher, and Pigeonpost. It covers the apps you offer to
buyers through the Nests marketplace.

## 1. Who we are and words we use

- **Piyote** means Piyote Bilişim ve Danışmanlık Hizmetleri Ticaret Ltd. Şti., Türkiye. Piyote is
  the seller of record for marketplace sales on the web. [Registered address, MERSIS number, tax
  office and number.]
- **Wodo** means Wodo Teknoloji A.Ş., Türkiye. Wodo operates the Pigeonpost service and its servers.
  [Registered address, MERSIS number, tax office and number.]
- **Pigeonpost**, **we** and **us** mean Piyote and Wodo, each for its own part: Piyote for sales,
  invoices, payouts and the commercial terms; Wodo for building, reviewing, hosting and running
  apps. [Counsel: confirm this split and which company signs this agreement.]
- **Nest** means an AI agent that Pigeonpost runs for a buyer in its own isolated sandbox.
- **Nest app** or **app** means software you publish that Pigeonpost builds and runs inside a
  buyer's Nest.
- **Release** means one version of an app, built from one exact source revision.
- **Buyer** means the person or organisation that buys an app and installs it into a Nest.
- **Installation** means one copy of an app installed in one Nest.
- **Marketplace terms** means the [Buyer Terms](buyer-terms.md), the
  [Refund Policy](refund-policy.md) and the
  [Acceptable Use and Review Policy](acceptable-use-and-review-policy.md).

## 2. Eligibility

- You must be able to enter into a binding contract.
- You must be an individual resident in Türkiye or a company established in Türkiye. [Later:
  publishers outside Türkiye.]
- You must hold a bank account in Türkiye, in your own name, that can receive TRY.
- You must give us true identity, tax and bank details, and keep them current. We may verify them
  and ask for documents.
- You must hold a Pigeonpost account with step-up sign-in enabled.
- We may refuse a publisher account. We do not have to give a reason, except where the law
  requires one.

## 3. Certification and review

- You submit source code. You never submit a built binary or image.
- Pigeonpost builds every release from source, in its own build environment.
- Every release goes through these stages:
  1. build from source, with a software bill of materials;
  2. automated scans (secrets, capabilities, network and external-write declarations, licences);
  3. an AI review on models that Pigeonpost runs itself. This review is advisory only. It never
     approves or rejects a release on its own;
  4. behaviour tests in a sandbox;
  5. a decision by Pigeonpost staff.
- Only a release approved by Pigeonpost staff can be sold or installed.
- Approval means the release passed our review at that time. It is not a warranty by us, and it
  does not move your responsibility for the app to us.
- We may ask for changes, reject a release, or approve it with conditions. We will tell you the
  reason, unless telling you would harm security, buyers or an investigation.
- You may appeal a decision as set out in the
  [Acceptable Use and Review Policy](acceptable-use-and-review-policy.md).
- Review takes [review time]. We do not promise a deadline.

## 4. Content and security rules

- Your app must follow the [Acceptable Use and Review Policy](acceptable-use-and-review-policy.md).
- Your app runs only inside the sandbox Pigeonpost gives it.
- Your app reaches the outside network only through the Pigeonpost broker, and only to the
  destinations declared in the reviewed manifest.
- Your app takes no action outside Pigeonpost (an "external write") unless the buyer approves that
  exact action first.
- Your app must do what its listing says, and nothing hidden.
- You must keep your app's dependencies maintained and fix security problems promptly.
- You must tell us at [security contact address] as soon as you learn of a security problem in your
  app.

## 5. Prohibited behaviour

You must not:

- try to escape, probe or weaken the sandbox, the broker or other Nests;
- include malware, hidden code paths, code that changes behaviour after review, or code that
  detects review or test environments;
- collect secrets, credentials or data that the app does not need for its listed purpose;
- send buyer data anywhere except as the listing and the buyer's approvals allow;
- sell buyer data, profile buyers, or use buyer data to train models, unless the buyer instructs it;
- mine cryptocurrency, send spam, or run unrelated workloads;
- mislead buyers about what the app does, who made it, its price or its review status;
- ask buyers to pay you outside the marketplace for something sold in the marketplace;
- manipulate ratings, reviews or rankings;
- infringe anyone's rights or break the law.

## 6. Data protection

### 6.1 Roles

- The buyer decides why and how its data is processed in its Nest. The buyer is the data
  controller (veri sorumlusu).
- Pigeonpost processes that data for the buyer as processor (veri işleyen).
- You process it as Pigeonpost's sub-processor, through your app. Where the law treats you as the
  buyer's processor directly, these terms apply in the same way.
- This applies under Law No. 6698 on the Protection of Personal Data (KVKK) and, where it applies,
  the EU and UK GDPR.
- [Counsel: confirm the roles, including for buyers who are individuals using an app for personal
  purposes, and whether a separate processing agreement is needed.]

### 6.2 Your obligations as processor

You must:

- process buyer data only to run the app as listed, and on the buyer's instructions as passed to
  you through the app and through Pigeonpost;
- keep buyer data confidential, and bind anyone working on it to confidentiality;
- keep appropriate technical and organisational security measures;
- use no other sub-processor (for example an outside service your app calls) unless it is declared
  in the manifest, shown in the listing and approved in review;
- tell Pigeonpost without undue delay, and within [breach notice period], after you learn of a
  personal data breach that affects buyer data;
- help Pigeonpost and the buyer answer requests from people about their data, and with any
  required impact assessment;
- delete buyer data you hold when the installation ends, and confirm it when we ask;
- give Pigeonpost the information it needs to show that these rules are kept, and allow reasonable
  audits;
- not transfer buyer data outside Türkiye unless the transfer is declared, approved and lawful.

### 6.3 What Pigeonpost gives you

- Pigeonpost gives you only redacted operation records for your installations: for example counts,
  status, error codes, timings and release versions. These records do not contain message content,
  file content, names, handles or contact addresses.
- You get no direct access to a buyer's Nest, its volume or its data.
- Nests are not end-to-end encrypted. Your app reads the data it processes inside the sandbox.
  This is why the rules above apply.

## 7. Intellectual property and licence

- You keep all rights in your app and its source code.
- You confirm that you have the rights needed to grant this licence, including for every
  dependency.
- You grant Pigeonpost a non-exclusive, worldwide, royalty-free licence, for the term of this
  agreement and the survival period below, to:
  - copy, build, scan, test and review your source code;
  - host, run and back up the built releases;
  - distribute and sell the app to buyers, and let buyers use it under the marketplace terms;
  - show your app's name, icon, screenshots and description in the marketplace and in promotion
    of the marketplace;
  - keep copies of source, builds and review evidence for security, audit, dispute and legal
    reasons.
- After termination, buyers who already paid for a one-off app may keep using the installed
  release for [survival period], and Pigeonpost may keep running it for them. [Owner and counsel:
  decide.]
- You grant each buyer the right to use the app in its Nests under the marketplace terms. [Any
  publisher end-user licence must not conflict with the marketplace terms.]
- You give Pigeonpost a licence to use any feedback you send, without payment.

## 8. Fees, commission, reserve and payouts

- The buyer pays Piyote. Piyote collects through iyzico.
- Prices are set by [you / us] within the limits we publish. Terms at launch are one-off and
  monthly only. There is no wallet and no prepaid credit.
- Piyote keeps a commission of [commission %] of [the net sale price, after VAT and payment costs /
  the gross price]. [Accountant: confirm the base.]
- Of your remaining share, Piyote holds back [reserve %] as a reserve for [hold days] after each
  sale. The reserve covers refunds, chargebacks and amounts you owe us. After that period the
  unused reserve is released to your next payout.
- Piyote pays you once a month, in TRY, by bank transfer to your Turkish bank account, on
  [payout day], for amounts that cleared in the previous month.
- Payouts below [minimum payout] roll over to the next month.
- Amounts from sales in another currency are converted to TRY at [exchange rate source and date].
- You must send any invoice or other tax document that the law requires before we pay. [Accountant:
  see the open questions.]
- We give you a monthly statement with sales, refunds, chargebacks, commission, reserve and
  payout.
- We may set off amounts you owe us against any payout.
- You may dispute a statement within [dispute window]. After that it is final, except for
  obvious errors.

## 9. Refunds and chargebacks

- Refunds follow the [Refund Policy](refund-policy.md). Piyote decides refunds as the seller.
- A refund or chargeback is taken from your share of that sale, from the reserve, or from a later
  payout.
- Piyote bears its commission on a refunded sale only if [the refund is caused by Pigeonpost /
  never]. [Owner: decide.]
- Chargeback fees charged by the payment provider are borne by [you / Piyote / shared]. [Owner:
  decide.]
- If your share does not cover what you owe, you must pay the difference within [payment period]
  after we ask.

## 10. Monthly apps and nonpayment

- A monthly app renews each month by card through iyzico, until the buyer cancels.
- If a renewal payment fails, the app's installation is paused at once at the due time. The buyer
  gets reminders. If the buyer still has not paid 72 hours later, the app's data in that
  installation is deleted.
- This rule applies only to marketplace apps. It never touches the buyer's Inboxes, handles,
  organisations or first-party Nest subscriptions.
- You get no payment for the unpaid period.

## 11. Taxes

- Each party pays its own taxes on its own income.
- Piyote charges and pays any VAT (KDV) due on sales to buyers and issues the buyer's invoice.
- EU and UK consumers cannot buy on the web yet. EU and UK businesses with a verified VAT number
  buy without VAT under the reverse charge.
- If you are an individual, Piyote may have to withhold income tax (stopaj) from payments to you
  and pay it to the tax office for you. [Accountant: confirm whether and how stopaj applies, and
  what documents an individual publisher must provide.]
- You are responsible for registering for tax where you must, and for your own declarations.

## 12. Suspension, quarantine and revocation

- We may quarantine a release, stop new sales, pause installations, or revoke a release, if:
  - it breaks this agreement or the marketplace terms;
  - it poses a security, privacy or legal risk;
  - the law or an authority requires it;
  - buyers report serious problems we confirm.
- **Quarantine** stops new installations and may pause existing ones while we investigate.
- **Revocation** removes the release. Installations stop running it.
- We act first and explain afterwards when there is an urgent risk. Otherwise we tell you first and
  give you [cure period] to fix the problem.
- We may suspend your publisher account for serious or repeated breaches.
- Refunds caused by quarantine or revocation for your breach are borne by your share.

## 13. Liability

- You are responsible for your app and for what it does.
- You will cover Pigeonpost against claims from buyers, people whose data was processed, or
  authorities that arise from your breach of this agreement, the law, or someone's rights.
- Pigeonpost runs the marketplace with reasonable care but does not promise it will be available at
  all times or free of errors.
- Neither party is liable for indirect or consequential loss, or for lost profit, except where the
  law does not allow this to be limited.
- Each party's total liability under this agreement in any [period] is limited to [liability cap].
  This limit does not apply to [your indemnity, data protection breaches, wilful misconduct, gross
  negligence, and anything the law does not allow to be limited].

## 14. Term and termination

- This agreement starts when you accept it and continues until it ends.
- You may end it with [notice period] notice. Your apps are removed from sale. Monthly
  installations [run to the end of their paid period / are moved to the Refund Policy rules].
- We may end it with [notice period] notice, or at once for a serious breach, fraud, or a legal
  requirement.
- On termination we pay you what is due, after the reserve period and any set-off.
- Sections 6, 7 (survival), 9, 11, 13 and 15 survive termination.

## 15. Other terms

- **Changes.** We may change this agreement. We give [notice period] notice of material changes.
  If you do not agree, you may end the agreement before the change takes effect.
- **Notices.** We send notices to the contact address on your publisher account, or as a
  Pigeonpost message. You send notices to [legal contact address].
- **Assignment.** You may not transfer this agreement without our consent. We may transfer it
  within our group or with the marketplace business.
- **Whole agreement.** This agreement and the documents it names are the whole agreement.
- **Language.** [This agreement is made in Turkish and English. If they differ, the [Turkish]
  version prevails.]
- **Governing law and courts.** This agreement is governed by [governing law]. The [courts] have
  jurisdiction.

## Özet (Türkçe)

> Taslaktır — yürürlükte değildir — hukuki inceleme gerekir.

- **Taraflar.** Web satışlarında satıcı Piyote Bilişim ve Danışmanlık Hizmetleri Ticaret Ltd. Şti.
  (Piyote); hizmeti ve sunucuları Wodo Teknoloji A.Ş. işletir. Hangi şirketin imzalayacağı avukatça
  netleştirilmelidir.
- **Uygunluk.** Türkiye'de yerleşik gerçek veya tüzel kişi, kendi adına Türk banka hesabı, doğru
  kimlik ve vergi bilgisi.
- **İnceleme.** Yayıncı yalnızca kaynak kodu verir. Pigeonpost her sürümü kaynaktan derler, tarar,
  kendi sunucularındaki yapay zekâ modelleriyle inceler (yalnızca tavsiye niteliğinde), davranış
  testinden geçirir; son kararı Pigeonpost personeli verir. Onay garanti değildir.
- **Kurallar.** Uygulama yalnızca kum havuzunda çalışır, dış ağa yalnızca Pigeonpost aracısı
  (broker) üzerinden çıkar, dış yazma işlemleri alıcının o işleme özel onayını gerektirir.
- **Kişisel veri.** Alıcı veri sorumlusu, Pigeonpost veri işleyen, yayıncı alt işleyendir (KVKK ve
  gerektiğinde GDPR). Yayıncıya yalnızca maskelenmiş işlem kayıtları verilir. Nest'ler uçtan uca
  şifreli değildir.
- **Lisans.** Fikri haklar yayıncıda kalır; Pigeonpost'a derleme, barındırma, dağıtım ve satış için
  münhasır olmayan lisans verilir.
- **Ödemeler.** Piyote [komisyon %] komisyon alır, yayıncı payının [rezerv %] kadarını [bekletme
  süresi] boyunca rezerv olarak tutar, kalan tutarı ayda bir TL olarak Türk banka hesabına havale
  eder. Cüzdan veya ön ödemeli kredi yoktur.
- **İade ve ters ibraz.** Yayıncının payından, rezervden veya sonraki ödemeden düşülür.
- **Aylık uygulama ödenmezse.** Vade anında durdurulur, hatırlatma yapılır, 72 saat sonra o
  kurulumdaki uygulama verisi silinir. Gelen kutuları, kullanıcı adları, kuruluşlar ve birinci
  taraf Nest abonelikleri etkilenmez.
- **Vergi.** Her taraf kendi vergisinden sorumludur. Gerçek kişi yayıncılara ödemelerde stopaj
  kesintisi gerekebilir — mali müşavir teyit etmelidir.
- **Askıya alma.** Pigeonpost sürümleri karantinaya alabilir, satışı durdurabilir veya geri
  çekebilir.
- **Sorumluluk, süre, fesih, uygulanacak hukuk:** yer tutucularla bırakılmıştır.
