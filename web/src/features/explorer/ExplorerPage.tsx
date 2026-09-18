import { Route, Routes } from 'react-router-dom';
import { SearchBar } from './SearchBar';
import { BlockList } from './BlockList';
import { BlockDetail } from './BlockDetail';
import { TransactionDetail } from './TransactionDetail';
import { AddressDetail } from './AddressDetail';

export function ExplorerPage() {
  return (
    <>
      <SearchBar />
      <Routes>
        <Route index element={<BlockList />} />
        <Route path="block/:id" element={<BlockDetail />} />
        <Route path="tx/:txid" element={<TransactionDetail />} />
        <Route path="address/:address" element={<AddressDetail />} />
      </Routes>
    </>
  );
}
