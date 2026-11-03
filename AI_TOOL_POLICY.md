# Rosemary AI Usage Policy

Contributors may use whatever tools they wish to create their contributions, but a human must be in the loop. Contributors must read and review all LLM-generated code or text before they ask other project members to review it. The contributor is always the author and is fully accountable for their actions.

As a consequence of our position that the contributor proposing changes is fully accountable for their contributions, contributors aren't required to explicitly mention the use of any tools, AI or otherwise. If a contributor thinks there is value in highlighting tool use, they may do so in their pull request description, commit message, or using a commit message trailer like Assisted-by: <tool>:<model>. They MUST NOT, however, list any tool, AI or otherwise, as a co-author: only human contributors can be authors and take on the accountability that comes with it.

## Details

To ensure sufficient self review and understanding of the work, it is strongly recommended that contributors write PR descriptions themselves (if needed, using tools for translation or copy-editing). The description should explain the motivation, implementation approach, expected impact, and any open questions or uncertainties to the same extent as a contribution made without tool assistance.

An important implication of this policy is that it bans agents that take action in our digital spaces without human approval, such as the GitHub @claude agent. Similarly, automated review tools that publish comments without human review are not allowed. However, an opt-in review tool that keeps a human in the loop is acceptable under this policy. As another example, using an LLM to generate documentation, which a contributor manually reviews for correctness, edits, and then posts as a PR, is an approved use of tools under this policy.

AI tools must not be used to fix GitHub issues labelled good first issue. These issues are generally not urgent, and are intended to be learning opportunities for new contributors to get familiar with the codebase. Whether you are a newcomer or not, fully automating the process of fixing this issue squanders the learning opportunity and doesn’t add much value to the project. Using AI tools to fix issues labelled as “good first issues” is forbidden.

## References

This policy is a subset of [Bytecode Alliance's own AI usage policy](https://github.com/bytecodealliance/governance/blob/9851b80b6c5d8aca959a8ad15820fc2be509aeb3/AI_TOOL_POLICY.md).
