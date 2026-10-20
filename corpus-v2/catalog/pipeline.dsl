# The pipeline class (PL-7.2): the provenance of what the agents submit, from
# the document a hypothesis came from to the forward reveals of its
# submission, and the gateway's audit. Written by the gateway alone through
# `put`, like the kernel's and the query layer's namespaces; no program reads
# it. Ids are labels: a row's identity is its labels with its time (D-2.2).
class pipeline {
  # A document, the hypotheses it led to, and the submissions that test them.
  document(DocId: Label, Source: Label, Author: Label, @P, Size: Count) complete   # published at P, Size bytes; the text stays in the ingest store
  hypothesis(Id: Label, DocIds: Label, @H, Agent: Label) complete   # DocIds is the JSON list of its documents; H its horizon
  submission(Id: Label, HypothesisId: Label, ProgramHash: Label, PlanHash: Label, Lineage: Label, @H) complete   # written once, at submit (PL-6.6): H(P), H(Π), the lineage and H

  # The forward reveals of a submission, and the gateway's audit (PL-7.1).
  outcome(SubmissionId: Label, @T, Reveal: Count, ForwardFrom: Timestamp, ForwardTo: Timestamp, Status: Label) complete   # the Reveal-th reveal, at T, over (ForwardFrom, ForwardTo]; Status insample_only, forward or retired
  audit(Job: Label, @T, Kind: Label, Subject: Label, Bytes: Count, DurationNs: Count, Outcome: Label) complete   # one per request at T: its kind, the relation or plan, the bytes, the time it took
}
