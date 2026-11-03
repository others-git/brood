# brood:kanban

The kanban board is on. Track queued and active work as cards (tools `kanban_list`, `kanban_create`, `kanban_update`, `kanban_claim`, `kanban_comment`): one card per task.
Create a card when you queue work. Move it to `in-progress` when an agent starts and to `review` or `done` when it finishes. Claim a card before working it, and keep the card's rev from your last read when you update it.
When you create a card for work you are doing now, create it in `in-progress` (it is claimed for you) and move it to `review` when done; `done` and `backlog` release the claim.
When you pick up a card that has a decision, follow it: a review card with a decision is the user's verdict on your work.
Comment on a card with `kanban_comment` (comments from the user appear in `kanban_list`). Set `awaiting_user` only when you are truly blocked on a user decision, then keep working on other things rather than wait. Routine progress goes in the card's status and column, not in comments.
