// Progressive enhancement only: all forms and navigation work without JavaScript.
(function () {
  'use strict';
  for (const form of document.querySelectorAll('form[data-confirm]')) {
    form.addEventListener('submit', function (event) {
      if (!window.confirm(form.dataset.confirm)) event.preventDefault();
    });
  }
  const flow = document.getElementById('flow-form');
  if (!flow || !flow.hasAttribute('data-original-sql')) return;
  flow.addEventListener('submit', function (event) {
    if (event.submitter?.getAttribute('formaction') === '/flows/test') return;
    const sql = flow.elements.namedItem('sql_query');
    const keys = flow.elements.namedItem('key_columns');
    if (!sql || !keys) return;
    const changed = sql.value !== flow.dataset.originalSql ||
      keys.value !== flow.dataset.originalKeys;
    if (changed && !window.confirm('SQL or identity columns changed. Review the baseline reset option before saving. Continue?')) {
      event.preventDefault();
    }
  });
}());
