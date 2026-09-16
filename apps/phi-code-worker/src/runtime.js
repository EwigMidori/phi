((inspect) => {
  const stringify = JSON.stringify;
  const keys = Object.keys;
  const ownKeys = Reflect.ownKeys;
  const create = Object.create;
  const setPrototypeOf = Object.setPrototypeOf;
  const descriptor = Object.getOwnPropertyDescriptor;
  const prototype = Object.getPrototypeOf;
  const objectPrototype = Object.prototype;
  const arrayIsArray = Array.isArray;
  const string = String;
  const finite = Number.isFinite;
  const makeSet = Set;
  const contains = Function.call.bind(Set.prototype.has);
  const add = Function.call.bind(Set.prototype.add);
  const remove = Function.call.bind(Set.prototype.delete);
  const encodingError = Error;
  const capture = globalThis.__capture;
  delete globalThis.__capture;
  function log(error, args) {
    for (let index = 0; index < args.length; index++) {
      if (index) capture(error, ' ');
      const arg = args[index];
      let message;
      try {
        message = typeof arg === 'string' ? arg : inspect(arg, { depth: 8, customInspect: false, indent: 2 });
      } catch (_) {
        // A throwing accessor/proxy must not discard other arguments or the calculation.
        message = '[Inspection failed]';
      }
      capture(error, message);
    }
    capture(error, '\n');
  }
  globalThis.console = Object.freeze({ log: (...args) => log(false, args), error: (...args) => log(true, args), warn: (...args) => log(true, args) });
  return (value, limit) => {
    let remaining = limit;
    const seen = new makeSet();
    function tagged(kind, value) {
      const result = create(null);
      result.kind = kind;
      if (value !== undefined) result.value = value;
      return result;
    }
    function visit(value, depth) {
      if (depth > 64 || --remaining < 0) throw new encodingError('Result exceeds encoding limit');
      if (value === null) return null;
      switch (typeof value) {
        case 'undefined': return tagged('undefined');
        case 'bigint': return tagged('bigint', string(value));
        case 'number': return finite(value) ? value : tagged('number', string(value));
        case 'boolean': return value;
        case 'string': remaining -= value.length; if (remaining < 0) throw new encodingError('Result exceeds encoding limit'); return value;
        case 'object': break;
        default: throw new encodingError('Unsupported return value: ' + typeof value);
      }
      if (contains(seen, value)) throw new encodingError('Circular return value');
      const array = arrayIsArray(value);
      if (!array && prototype(value) !== objectPrototype && prototype(value) !== null) throw new encodingError('Only plain objects and arrays can be returned');
      add(seen, value);
      const allKeys = ownKeys(value);
      for (let index = 0; index < allKeys.length; index++) if (typeof allKeys[index] === 'symbol') throw new encodingError('Symbol return properties are unsupported');
      const result = array ? [] : create(null);
      if (array) setPrototypeOf(result, null);
      if (array) {
        if (value.length > remaining) throw new encodingError('Result exceeds encoding limit');
        for (let index = 0; index < value.length; index++) {
          const property = descriptor(value, string(index));
          if (!property || !('value' in property)) throw new encodingError('Sparse arrays and accessors are unsupported');
        }
      }
      const names = keys(value);
      for (let index = 0; index < names.length; index++) {
        const key = names[index];
        if (array && (string(+key) !== key || +key >= value.length || +key < 0 || +key % 1 !== 0)) throw new encodingError('Named array properties are unsupported');
        const property = descriptor(value, key);
        if (!property || !('value' in property)) throw new encodingError('Accessor return properties are unsupported');
        remaining -= key.length;
        result[key] = visit(property.value, depth + 1);
      }
      if (array && result.length !== value.length) throw new encodingError('Sparse arrays are unsupported');
      remove(seen, value);
      return result;
    }
    const encoded = stringify(visit(value, 0));
    if (encoded.length > limit) throw new encodingError('Result exceeds encoding limit');
    return encoded;
  };
})
