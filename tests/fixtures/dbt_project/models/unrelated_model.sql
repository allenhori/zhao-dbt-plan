-- Deliberately outside tag:microbatch_demo and configured with no
-- config.meta.zhao at all -- proves --select actually filters, rather
-- than the plan just including every model in the project regardless of
-- the selector.
select 1 as id
