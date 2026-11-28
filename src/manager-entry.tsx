import ReactDOM from 'react-dom/client';
import { ManagerApp } from './components/manager/ManagerApp';
import './index.css';

document.documentElement.dataset.theme = 'mono';
ReactDOM.createRoot(document.getElementById('root')!).render(<ManagerApp />);
